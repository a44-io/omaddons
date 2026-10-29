//! `adopt --apply`, `adopt <folder>…` and `adopt --ref R <folder>`: the report's groups
//! become one records-only `adopt` transaction per target (`RecordEdit::Adopt`), and a
//! managed folder named with `--ref` has its addon re-pointed (`RecordEdit::Repoint`).
//!
//! Folders are indexed without the lock. The engine's guard re-checks them under it, so
//! a file changed in between is `plan_stale` and nothing of that target is recorded. One
//! run id covers every target, so a bare `undo` reverts the whole adoption.
//! An adopting run's `--dry-run` returns `data.precondition`, and `--if-match <it>`
//! adopts exactly what that preview showed or nothing (M4-D19): the token covers each
//! adopted folder's content. The report takes no `--if-match`.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::json;

use super::{AdoptArgs, GroupWork, TargetWork, index_group, toc_version};
use crate::app::approval::{Approval, ApprovalFlags};
use crate::app::engine::Engine;
use crate::app::intent::NormalizedRef;
use crate::app::outcome::{
    ItemDeps, ItemError, ItemRequest, ItemResult, ItemStatus, TargetRef, TargetsData,
};
use crate::app::precondition::{self, Idle, PlanBinding, PlanExtras, Precondition};
use crate::app::resolve_args::{for_family, parse_ref};
use crate::app::world::World;
use crate::cli::{Context, fix_argv, without_flag};
use crate::discover::DiscoveredTarget;
use crate::discover::registry::{resolve_one_in, with_target};
use crate::engine::edit::{AdoptSpec, RecordEdit};
use crate::engine::plan::{Plan, RequestedChange};
use crate::engine::{TxRequest, TxTarget};
use crate::error::{Error, ErrorCode, Result, Suggestion, Warning, WarningCode};
use crate::inventory::FolderClass;
use crate::limits::ADOPT_MAX_ALTERNATIVES;
use crate::model::{
    AddonReason, AddonRecord, AddonState, FolderName, Ref, RunId, TxId, TxKind, VersionConfidence,
};
use crate::resolve::adopt::{GroupClaim, Strength, Tier};
use crate::resolve::cache::ReleaseCache;
use crate::resolve::catalog::Catalog;
use crate::rules::Family;
use crate::state::memo::CandidateFact;
use crate::state::records::TargetRecord;

/// What `adopt` was asked to do.
pub(super) enum Mode<'a> {
    /// No folders, no `--apply`: report only.
    Report,
    /// Every `exact` and `identified` group.
    Apply,
    /// The groups holding these folders, any tier from `guess` up.
    Folders(&'a [String]),
    /// One folder's group from `ref`, or its managed addon re-pointed there.
    Ref { folder: &'a str, r#ref: &'a str },
}

impl<'a> Mode<'a> {
    /// The mode `args` ask for; `--apply` with folders, and `--ref` without exactly one
    /// folder, are usage errors.
    pub(super) fn of(ctx: &Context, args: &'a AdoptArgs) -> Result<Self> {
        if args.apply && !args.folders.is_empty() {
            let argv: Vec<String> = fix_argv(&ctx.argv)
                .into_iter()
                .filter(|a| a != "--apply")
                .collect();
            return Err(Error::new(
                ErrorCode::Usage,
                "--apply adopts every exact and identified addon: name folders or give --apply, not both",
                Suggestion::command(argv),
            ));
        }
        // `adopt apply` (M4-D27): the flag the user meant, not a folder named `apply`
        // (one really named so is adopted with `--ref`).
        if !args.apply
            && args.r#ref.is_none()
            && let [word] = args.folders.as_slice()
            && word.eq_ignore_ascii_case("apply")
        {
            let fix = crate::cli::fix_argv_replacing(&ctx.argv, word, "--apply")
                .unwrap_or_else(|| vec!["omaddons".into(), "adopt".into(), "--apply".into()]);
            return Err(Error::new(
                ErrorCode::Usage,
                "'apply' is a flag of adopt here: adopt --apply adopts every exact and identified addon",
                Suggestion::command(fix),
            )
            .user_verb());
        }
        let mode = match (&args.r#ref, args.folders.as_slice()) {
            (Some(r), [folder]) => Ok(Self::Ref {
                folder: folder.as_str(),
                r#ref: r.as_str(),
            }),
            (Some(_), _) => Err(Error::new(
                ErrorCode::Usage,
                "--ref says where one folder comes from: name exactly one",
                Suggestion::text("omaddons adopt --ref <ref> <folder>"),
            )),
            (None, []) if args.apply => Ok(Self::Apply),
            (None, []) => Ok(Self::Report),
            (None, folders) => Ok(Self::Folders(folders)),
        }?;
        report_takes_no_token(
            matches!(mode, Self::Report),
            ctx.if_match.as_deref(),
            &ctx.argv,
        )?;
        Ok(mode)
    }
}

/// The report adopts nothing and returns no token. Its fix keeps the report, without
/// the flag, so a mistaken invocation never suggests an unrequested adoption.
fn report_takes_no_token(report: bool, if_match: Option<&str>, argv: &[String]) -> Result<()> {
    if report && if_match.is_some() {
        return Err(Error::new(
            ErrorCode::Usage,
            "--if-match binds a run that adopts (--apply, folder names or --ref); the report adopts nothing, so it takes no token",
            Suggestion::command(fix_argv(&without_flag(argv, "--if-match"))),
        )
        .user_verb());
    }
    Ok(())
}

/// What adopting does with one request on one target.
pub(super) enum Act {
    /// Adopts the group at this index of the target's groups.
    Adopt {
        group: usize,
        request: String,
    },
    /// Re-points the managed addon `addon` to `to`.
    Repoint {
        request: String,
        addon: String,
        to: Ref,
        version: String,
    },
    /// Already so: a managed folder named without `--ref`, or `--ref` its own ref.
    Unchanged {
        request: String,
        addon: String,
        r#ref: Ref,
        version: String,
    },
    Fail {
        request: String,
        error: Error,
    },
}

/// `--ref`'s ref and the catalog's name for it, when the catalog lists it.
pub(super) struct Override {
    r#ref: Ref,
    name: Option<String>,
    /// The same parsed override, retained for a stored preview without reparsing it.
    pub normalized: std::result::Result<NormalizedRef, String>,
}

/// `request` read as `install` reads an argument (catalog names and aliases, URLs,
/// paths, installed names), on a client of `family` (a catalog name with variants names
/// that family's).
pub(super) fn parse_override(
    world: &World,
    records: &BTreeMap<String, TargetRecord>,
    catalog: &Catalog,
    (request, family): (&str, Family),
) -> Result<Override> {
    let cwd = std::env::current_dir().unwrap_or_default();
    let parsed = parse_ref(&world.config.git_hosts, records, catalog, &cwd, request)?;
    let parsed = for_family(&parsed, catalog, family, request)?;
    let name = catalog
        .entries()
        .iter()
        .find(|e| e.r#ref.identity() == parsed.r#ref.identity())
        .map(|e| e.name.clone());
    Ok(Override {
        normalized: NormalizedRef::from_parsed(request, &parsed),
        r#ref: parsed.r#ref,
        name,
    })
}

/// The one target `--ref` acts on: `-t`'s, else the only one whose AddOns holds
/// `folder` (several are `target_ambiguous`, with a `-t` fix for each).
pub(super) fn ref_target<'w>(
    ctx: &Context,
    world: &'w World,
    folder: &str,
) -> Result<&'w DiscoveredTarget> {
    let targets = &world.resolved.targets;
    if let Some(selector) = &ctx.target {
        return resolve_one_in(selector, &world.resolved, &world.table, &ctx.argv);
    }
    let mut having: Vec<&DiscoveredTarget> = targets
        .iter()
        .filter(|d| holds(&d.target.addons_dir(), folder))
        .collect();
    having.sort_by(|a, b| a.target.id.cmp(&b.target.id));
    match having.as_slice() {
        [one] => Ok(one),
        [] => Err(Error::new(
            ErrorCode::NotFound,
            format!("{folder} is in no client's AddOns"),
            Suggestion::command(["omaddons", "list", "--unmanaged"]),
        )),
        [first, rest @ ..] => {
            let ids: Vec<&str> = having.iter().map(|d| d.target.id.as_str()).collect();
            let mut error = Error::new(
                ErrorCode::TargetAmbiguous,
                format!("{folder} is on {} clients: {}", ids.len(), ids.join(", ")),
                Suggestion::command(with_target(&ctx.argv, &first.target.id)),
            )
            .details(json!({ "folder": folder, "candidates": ids }));
            for d in rest {
                error =
                    error.alternative(Suggestion::command(with_target(&ctx.argv, &d.target.id)));
            }
            Err(error)
        }
    }
}

/// `addons` has an entry named `folder`, ignoring ASCII case.
fn holds(addons: &std::path::Path, folder: &str) -> bool {
    std::fs::read_dir(addons).is_ok_and(|listing| {
        listing
            .filter_map(std::result::Result::ok)
            .any(|e| e.file_name().to_string_lossy().eq_ignore_ascii_case(folder))
    })
}

/// What the mode adopts on each target, as acts, with each adopted group marked chosen.
pub(super) fn choose(
    mode: &Mode<'_>,
    works: &mut [TargetWork<'_>],
    records: &BTreeMap<String, TargetRecord>,
    over: Option<&Override>,
    argv: &[String],
) {
    match mode {
        Mode::Report => {}
        Mode::Apply => {
            for work in works.iter_mut() {
                for (g, gw) in work.groups.iter().enumerate() {
                    if matches!(gw.group.tier, Tier::Exact | Tier::Identified) {
                        work.acts.push(Act::Adopt {
                            group: g,
                            request: gw.group.primary.as_str().to_owned(),
                        });
                    }
                }
            }
        }
        Mode::Folders(folders) => {
            // A folder on none of the targets fails on each; on some, it is adopted there.
            let mut found = vec![false; folders.len()];
            for work in works.iter_mut() {
                let record = records.get(&work.discovered.target.id);
                for (i, folder) in folders.iter().enumerate() {
                    if let Some(act) = choose_folder(work, folder, record, None, argv) {
                        work.acts.push(act);
                        found[i] = true;
                    }
                }
            }
            for (folder, _) in folders.iter().zip(found).filter(|(_, found)| !found) {
                for work in works.iter_mut() {
                    let act = absent(folder, &work.discovered.target.id);
                    work.acts.push(act);
                }
            }
        }
        Mode::Ref { folder, .. } => {
            for work in works.iter_mut() {
                let record = records.get(&work.discovered.target.id);
                let act = choose_folder(work, folder, record, over, argv)
                    .unwrap_or_else(|| absent(folder, &work.discovered.target.id));
                work.acts.push(act);
            }
        }
    }
    for work in works.iter_mut() {
        for act in &work.acts {
            if let Act::Adopt { group, .. } = act
                && let Some(gw) = work.groups.get_mut(*group)
            {
                gw.chosen = true;
            }
        }
    }
}

/// `not_found` for a folder not in this target's AddOns.
fn absent(folder: &str, id: &str) -> Act {
    Act::Fail {
        request: folder.to_owned(),
        error: Error::new(
            ErrorCode::NotFound,
            format!("{folder} is not in AddOns on {id}"),
            Suggestion::command(["omaddons", "list", "-t", id, "--unmanaged"]),
        ),
    }
}

/// What naming `request` does on `work`'s target; `None` when its AddOns has no such
/// entry.
fn choose_folder(
    work: &mut TargetWork<'_>,
    request: &str,
    record: Option<&TargetRecord>,
    over: Option<&Override>,
    argv: &[String],
) -> Option<Act> {
    let id = work.discovered.target.id.clone();
    let entry = work
        .entries
        .iter()
        .find(|e| e.name.eq_ignore_ascii_case(request))?;
    let (name, class, owners, duplicate, owned_on) = (
        entry.name.clone(),
        entry.class,
        entry.owners.clone(),
        entry.case_duplicate,
        entry.owned_on.first().cloned(),
    );
    let fail = |error: Error| {
        Some(Act::Fail {
            request: request.to_owned(),
            error,
        })
    };
    let list = |flag: &str| Suggestion::command(["omaddons", "list", "-t", id.as_str(), flag]);
    let not_addon = |what: &str, class: &str| {
        Error::new(
            ErrorCode::UnmanagedFolder,
            format!("{name} is {what}; only real addon folders are adopted"),
            list("--unmanaged"),
        )
        .details(json!({ "folder": name, "class": class }))
    };
    match class {
        FolderClass::Protected => {
            return fail(Error::new(
                ErrorCode::ProtectedFolder,
                format!("{name} belongs to the game client and is never adopted"),
                list("--protected"),
            ));
        }
        FolderClass::Linked => return fail(not_addon("a symlink", "linked")),
        FolderClass::Invalid => return fail(not_addon("not an addon folder", "invalid")),
        FolderClass::Managed => return Some(managed(request, &owners, record, over, &id)),
        // Recorded by a client sharing this AddOns directory: one record per folder.
        FolderClass::Shared => {
            let owned = owned_on?;
            return fail(crate::engine::ownership::owned_on_shared(
                &name, &owned, argv,
            ));
        }
        FolderClass::Unmanaged if duplicate => {
            return fail(not_addon(
                "one of several folders differing only in case",
                "unmanaged",
            ));
        }
        FolderClass::Unmanaged => {}
    }
    let key = name.to_ascii_lowercase();
    let holds = |folders: &[FolderName]| folders.iter().any(|f| f.key() == key);
    let in_group = work.groups.iter().position(|gw| holds(&gw.group.folders));
    let leftover = work.leftovers.iter().position(|(g, _)| holds(&g.folders));

    if let Some(over) = over {
        if let Some(owner) = super::recorded_as(record, &over.r#ref) {
            return fail(already_recorded(&over.r#ref, &owner, &id));
        }
        let g = match (in_group, leftover) {
            (Some(g), _) => g,
            (None, Some(l)) => {
                let (group, _) = work.leftovers.remove(l);
                work.groups.push(GroupWork::new(group));
                work.groups.len() - 1
            }
            (None, None) => return fail(unknown(&name, &id)),
        };
        let folder = match FolderName::new(&name) {
            Ok(folder) => folder,
            Err(why) => return fail(not_addon(&why.to_string(), "invalid")),
        };
        let gw = work.groups.get_mut(g)?;
        override_group(gw, &folder, over);
        return Some(Act::Adopt {
            group: g,
            request: request.to_owned(),
        });
    }

    let Some(g) = in_group else {
        return match leftover.and_then(|l| work.leftovers.get(l)) {
            Some((_, owner)) => fail(
                Error::new(
                    ErrorCode::OwnershipConflict,
                    format!("{name} is a leftover of {owner}, which is already managed on {id}"),
                    Suggestion::text("remove the leftover yourself, or pass the ref it belongs to"),
                )
                .details(json!({ "folder": name, "owner": owner })),
            ),
            None => fail(unknown(&name, &id)),
        };
    };
    let gw = work.groups.get(g)?;
    let adopt = Some(Act::Adopt {
        group: g,
        request: request.to_owned(),
    });
    match gw.group.tier {
        Tier::Exact | Tier::Identified => adopt,
        Tier::Guess if gw.group.alternatives.is_empty() => adopt,
        Tier::Guess => {
            let refs = ref_choices(
                gw.group.claim.as_ref().and_then(|c| c.r#ref.as_ref()),
                &gw.group.alternatives,
            );
            let shown: Vec<String> = refs.iter().map(ToString::to_string).collect();
            let mut argvs = refs.iter().map(|r| with_ref_argv(&name, r, &id));
            let first = argvs.next().unwrap_or_default();
            let mut error = Error::new(
                ErrorCode::RefAmbiguous,
                format!(
                    "{name} may come from {}: choose with --ref",
                    shown.join(" or ")
                ),
                Suggestion::command(first),
            )
            .details(json!({ "folder": name, "refs": shown }));
            for argv in argvs {
                error = error.alternative(Suggestion::command(argv));
            }
            fail(error)
        }
        Tier::CfOnly => fail(Error::new(
            ErrorCode::SourceDisabled,
            format!("{name} is known only to CurseForge or Wago, which arrive in M5"),
            ref_text(&name, &id),
        )),
        Tier::Unknown => fail(unknown(&name, &id)),
    }
}

/// A managed folder named: its addon unchanged, or re-pointed to `--ref`'s ref.
fn managed(
    request: &str,
    owners: &[String],
    record: Option<&TargetRecord>,
    over: Option<&Override>,
    id: &str,
) -> Act {
    let addon = owners
        .first()
        .and_then(|o| record?.addons.get(&TargetRecord::addon_key(o)));
    let Some(a) = addon else {
        return Act::Fail {
            request: request.to_owned(),
            error: crate::engine::edit::not_managed(request, id),
        };
    };
    let unchanged = || Act::Unchanged {
        request: request.to_owned(),
        addon: a.name.clone(),
        r#ref: a.r#ref.clone(),
        version: a.version.clone(),
    };
    let Some(over) = over else {
        return unchanged();
    };
    if a.r#ref.identity() == over.r#ref.identity() {
        return unchanged();
    }
    if let Some(owner) = super::recorded_as(record, &over.r#ref) {
        return Act::Fail {
            request: request.to_owned(),
            error: already_recorded(&over.r#ref, &owner, id),
        };
    }
    Act::Repoint {
        request: request.to_owned(),
        addon: a.name.clone(),
        to: over.r#ref.clone(),
        version: a.version.clone(),
    }
}

/// `ownership_conflict`: `r` is recorded on `id` already, by `owner`.
fn already_recorded(r: &Ref, owner: &str, id: &str) -> Error {
    Error::new(
        ErrorCode::OwnershipConflict,
        format!("{r} is already recorded on {id}, as {owner}: one ref, one addon"),
        Suggestion::text("remove the leftover yourself, or pass the ref it belongs to"),
    )
    .details(json!({ "owner": owner }))
}

/// `not_found` for a folder nothing identifies: the ref is the user's to name; what
/// ships the folder is a read-only search away.
/// `wrong_flavor`: every folder of `gw` is one its release `r` ships but this client does
/// not load, so adopting it would record no folder.
fn not_loaded(gw: &GroupWork, r: &Ref, id: &str) -> Error {
    let folders: Vec<&str> = gw.group.folders.iter().map(FolderName::as_str).collect();
    Error::new(
        ErrorCode::WrongFlavor,
        format!(
            "{r} installs {} only on other clients: {id} does not load it",
            folders.join(", ")
        ),
        Suggestion::command(["omaddons", "list", "-t", id, "--unmanaged"]),
    )
    .details(json!({ "unloaded_folders": folders }))
}

fn unknown(name: &str, id: &str) -> Error {
    Error::new(
        ErrorCode::NotFound,
        format!("nothing says where {name} comes from"),
        ref_text(name, id),
    )
    .alternative(Suggestion::command(["omaddons", "search", name]))
}

/// The text fix that names the ref to fill in.
fn ref_text(folder: &str, id: &str) -> Suggestion {
    Suggestion::text(format!(
        "omaddons adopt --ref <ref> {} -t {}",
        shell_words::quote(folder),
        shell_words::quote(id)
    ))
}

/// A group's ref and its alternatives, at most `ADOPT_MAX_ALTERNATIVES` in all.
pub(super) fn ref_choices(claimed: Option<&Ref>, alternatives: &[Ref]) -> Vec<Ref> {
    claimed
        .into_iter()
        .chain(alternatives)
        .take(ADOPT_MAX_ALTERNATIVES)
        .cloned()
        .collect()
}

/// `omaddons adopt <folder> --ref <r> -t <id>`.
pub(super) fn with_ref_argv(folder: &str, r: &Ref, id: &str) -> Vec<String> {
    [
        "omaddons",
        "adopt",
        folder,
        "--ref",
        &r.to_string(),
        "-t",
        id,
    ]
    .map(str::to_owned)
    .to_vec()
}

/// `--ref` decides the group: its ref, the catalog's name for it (else the claim's when
/// it named the same ref), `identified`, no rivals. A group nothing identified is just
/// the folder named.
fn override_group(gw: &mut GroupWork, folder: &FolderName, over: &Override) {
    let group = &mut gw.group;
    if group.tier == Tier::Unknown {
        group.folders = vec![folder.clone()];
        group.primary = folder.clone();
    }
    let old = group.claim.take();
    let same = old
        .as_ref()
        .and_then(|c| c.r#ref.as_ref())
        .is_some_and(|r| r.identity() == over.r#ref.identity());
    let name = over.name.clone().or_else(|| {
        same.then(|| old.as_ref().and_then(|c| c.name.clone()))
            .flatten()
    });
    group.claim = Some(GroupClaim {
        r#ref: Some(over.r#ref.clone()),
        strength: old.as_ref().map_or(Strength::NameOnly, |c| c.strength),
        evidence: old.map(|c| c.evidence).unwrap_or_default(),
        name,
    });
    group.tier = Tier::Identified;
    group.alternatives.clear();
    gw.reason = Some(super::AdoptReason::NotVerified);
}

/// What adopting committed.
pub(super) struct Committed {
    pub data: TargetsData,
    pub warnings: Vec<Warning>,
    pub notes: Vec<String>,
    pub locks: Vec<(TxId, u64)>,
    pub facts: Vec<CandidateFact>,
    /// The dry run's exact plan and binding, before its parent builds the intent.
    pub preview_plan: Option<Box<(Plan, Precondition)>>,
    /// Each failure decided here, by target id and request: with its details and
    /// alternatives, which an item's error does not keep.
    errors: Vec<(String, String, Error)>,
}

impl Committed {
    /// When every item failed: the error to fail the command with. A single failure
    /// decided here keeps its own error (alternatives included).
    pub fn failure(&self) -> Option<Error> {
        let all = self.data.all_failed()?;
        let failed: Vec<(&str, &str)> = self
            .data
            .targets
            .iter()
            .flat_map(|t| {
                t.items
                    .iter()
                    .filter(|i| i.status == ItemStatus::Failed)
                    .map(|i| (t.target.id.as_str(), i.request.as_str()))
            })
            .collect();
        if let [(target, request)] = failed.as_slice()
            && let Some((_, _, error)) = self
                .errors
                .iter()
                .find(|(t, r, _)| t.as_str() == *target && r.as_str() == *request)
        {
            return Some(error.clone());
        }
        Some(all)
    }
}

/// Builds each target's adoption (indexing what is not indexed yet) and commits them,
/// one `adopt` transaction per target, under one run.
pub(super) fn commit(
    ctx: &Context,
    engine: &Engine,
    cache: &ReleaseCache<'_>,
    works: Vec<TargetWork<'_>>,
    records: &BTreeMap<String, TargetRecord>,
) -> Result<Committed> {
    let mut out = Committed {
        data: TargetsData::assemble(&[], &[], &[], ctx.dry_run, RunId::new(), &[]),
        warnings: Vec::new(),
        notes: Vec::new(),
        locks: Vec::new(),
        facts: Vec::new(),
        preview_plan: None,
        errors: Vec::new(),
    };
    let limits = engine.archive_limits();
    let channel = engine.default_channel();
    let mut requested: Vec<ItemRequest> = Vec::new();
    let mut decided: Vec<(TargetRef, ItemResult)> = Vec::new();
    let mut tx_targets: Vec<TxTarget> = Vec::new();
    let scope = noop_scope(&works, records);
    let flags = ApprovalFlags {
        while_running: ctx.while_running,
        ..ApprovalFlags::default()
    };
    for work in works {
        let TargetWork {
            discovered,
            candidates,
            mut groups,
            acts,
            ..
        } = work;
        let target = &discovered.target;
        let id = target.id.clone();
        let target_ref = TargetRef {
            id: id.clone(),
            key: target.key,
            mode: None,
        };
        let addons = target.addons_dir();
        let mut taken: BTreeSet<String> = records
            .get(&id)
            .map(|r| r.addons.keys().cloned().collect())
            .unwrap_or_default();
        // Each group adopted once: a second folder of it collapses into the first.
        let mut adopted: BTreeMap<usize, (String, Ref)> = BTreeMap::new();
        let mut changes: Vec<RequestedChange> = Vec::new();
        let mut failures: Vec<(String, Error)> = Vec::new();
        for act in acts {
            match act {
                Act::Adopt { group, request } => {
                    if let Some((name, r)) = adopted.get(&group) {
                        requested.push(ItemRequest {
                            request,
                            target: id.clone(),
                            addon: Some(name.clone()),
                            r#ref: Some(r.clone()),
                            version: None,
                        });
                        continue;
                    }
                    let Some(gw) = groups.get_mut(group) else {
                        continue;
                    };
                    index_group(gw, &addons, &limits);
                    if let Some(error) = &gw.index_error {
                        let error = Error::new(
                            error.code,
                            error.message.clone(),
                            Suggestion::command(["omaddons", "list", "-t", &id, "--unmanaged"]),
                        )
                        .details(error.details.clone());
                        failures.push((request, error));
                        continue;
                    }
                    let Some(r) = gw.group.claim.as_ref().and_then(|c| c.r#ref.clone()) else {
                        failures.push((request, unknown(gw.group.primary.as_str(), &id)));
                        continue;
                    };
                    // Folders its release installs only on other clients: a record of
                    // them would hold no folder.
                    if gw
                        .group
                        .folders
                        .iter()
                        .all(|f| gw.unloaded.iter().any(|u| u.key() == f.key()))
                    {
                        failures.push((request, not_loaded(gw, &r, &id)));
                        continue;
                    }
                    let name = record_name(gw, &id, &mut taken, &mut out.warnings);
                    let record = adopted_record(gw, &r, &name, &candidates, channel);
                    let folders = record
                        .folders
                        .iter()
                        .filter_map(|f| Some((f.clone(), gw.indexes.get(f.key())?.clone())))
                        .collect();
                    changes.push(RequestedChange::Edit {
                        addon: name.clone(),
                        edit: RecordEdit::Adopt(Box::new(AdoptSpec { record, folders })),
                    });
                    requested.push(ItemRequest {
                        request,
                        target: id.clone(),
                        addon: Some(name.clone()),
                        r#ref: Some(r.clone()),
                        version: None,
                    });
                    adopted.insert(group, (name, r));
                }
                Act::Repoint {
                    request,
                    addon,
                    to,
                    version,
                } => {
                    changes.push(RequestedChange::Edit {
                        addon: addon.clone(),
                        edit: RecordEdit::Repoint(to.clone()),
                    });
                    requested.push(ItemRequest {
                        request,
                        target: id.clone(),
                        addon: Some(addon),
                        r#ref: Some(to),
                        version: Some(version),
                    });
                }
                Act::Unchanged {
                    request,
                    addon,
                    r#ref,
                    version,
                } => decided.push((
                    target_ref.clone(),
                    ItemResult {
                        request,
                        name: Some(addon),
                        r#ref: Some(r#ref),
                        status: ItemStatus::Unchanged,
                        skip_reason: None,
                        duplicate_of: None,
                        reason: None,
                        from_version: Some(version.clone()),
                        to_version: Some(version),
                        folders: None,
                        required_by: None,
                        deps: ItemDeps::default(),
                        error: None,
                    },
                )),
                Act::Fail { request, error } => failures.push((request, error)),
            }
        }
        for (request, error) in failures {
            decided.push((target_ref.clone(), failed_item(request.clone(), &error)));
            out.errors.push((id.clone(), request, error));
        }
        if !changes.is_empty() {
            tx_targets.push(TxTarget {
                discovered: discovered.clone(),
                changes,
                default_kind: TxKind::Adopt,
                dependency_locks: BTreeMap::new(),
            });
        }
    }

    if !tx_targets.is_empty() {
        let run = RunId::new();
        let req = TxRequest {
            targets: tx_targets,
            force: false,
            while_running: ctx.while_running,
            dry_run: ctx.dry_run,
            reason: ctx.reason.clone(),
            kind: Some(TxKind::Adopt),
            run,
            no_deps: true,
            witness: precondition::witnessing(ctx),
        };
        let extras = PlanExtras::default();
        let binding = PlanBinding::of_run(ctx, "adopt", extras.clone());
        let done = engine.run_bound(
            &req,
            Some(cache),
            binding.as_ref().map(|b| b as &dyn crate::engine::Binding),
        )?;
        out.facts.extend(done.plan.facts.iter().cloned());
        out.data = TargetsData::build(&done, run, &requested);
        let plan_precondition = ctx
            .dry_run
            .then(|| precondition::plan_precondition("adopt", &done.plan, &extras));
        out.data.precondition = plan_precondition.as_ref().map(Precondition::token);
        if ctx.dry_run {
            out.data = out
                .data
                .with_approval(Some(Approval::of_plan(&done.plan, records, &flags)?));
        }
        for o in &done.outcomes {
            out.warnings.extend(o.warnings.iter().cloned());
            out.notes.extend(o.notes.iter().cloned());
            if let (Some(tx), Some(ms)) = (o.tx, o.lock_held_ms) {
                out.locks.push((tx, ms));
            }
        }
        out.warnings.extend(done.warnings.iter().cloned());
        if let Some(p) = plan_precondition.filter(|_| done.plan.has_ops()) {
            out.preview_plan = Some(Box::new((done.plan, p)));
        }
    } else {
        // Even a run with nothing to adopt checks its token before any recovery.
        out.data.precondition =
            precondition::check_noop(ctx, "adopt", &scope, &PlanExtras::default())?;
        engine.recover_waiting(ctx);
        if ctx.dry_run {
            out.data = out.data.with_approval(Some(Approval::nothing(&flags)));
        }
    }
    for (target, item) in decided {
        out.data.push_item(target, item);
    }
    for t in &out.data.targets {
        for i in t.items.iter().filter(|i| i.status == ItemStatus::Failed) {
            if let Some(e) = &i.error {
                let mut warning = Warning::item(&e.error());
                warning.message = format!("{}: {}", t.target.id, e.message);
                out.warnings.push(warning);
            }
        }
    }
    Ok(out)
}

/// Every client examined to decide there was nothing to adopt, including clients
/// not yet registered, which the token names by provisional id at generation zero.
fn noop_scope<'a>(
    works: &[TargetWork<'a>],
    records: &BTreeMap<String, TargetRecord>,
) -> Vec<Idle<'a>> {
    works
        .iter()
        .map(|work| Idle {
            target: &work.discovered.target,
            generation: records
                .get(&work.discovered.target.id)
                .map_or(0, |r| r.generation),
        })
        .collect()
}

/// The name the group's record takes: its claim's, else its primary folder's. A name
/// another addon on the target has becomes `<name> (<primary folder>)`, with an
/// `adopt_name_changed` warning.
fn record_name(
    gw: &GroupWork,
    id: &str,
    taken: &mut BTreeSet<String>,
    warnings: &mut Vec<Warning>,
) -> String {
    let primary = gw.group.primary.as_str();
    let base = gw
        .group
        .claim
        .as_ref()
        .and_then(|c| c.name.clone())
        .unwrap_or_else(|| primary.to_owned());
    let name = if taken.contains(&TargetRecord::addon_key(&base)) {
        let renamed = format!("{base} ({primary})");
        warnings.push(
            Warning::new(
                WarningCode::AdoptNameChanged,
                format!("{base} is already an addon on {id}: {primary} is recorded as {renamed}"),
            )
            .with_fix(Suggestion::command(["omaddons", "list", "-t", id])),
        );
        renamed
    } else {
        base
    };
    taken.insert(TargetRecord::addon_key(&name));
    name
}

/// The record adopting `gw` writes: the release's version when exact, else the TOC's;
/// its folders but those the client does not load. `installed_at` and `tx_id` are the
/// commit's.
fn adopted_record(
    gw: &GroupWork,
    r: &Ref,
    name: &str,
    candidates: &[crate::resolve::adopt::CandidateFolder],
    channel: crate::model::Channel,
) -> AddonRecord {
    let unloaded: BTreeSet<&str> = gw.unloaded.iter().map(FolderName::key).collect();
    let (version, version_confidence, release_id, blob_sha256) = match &gw.exact {
        Some(exact) => (
            exact.version.clone(),
            VersionConfidence::Release,
            Some(exact.release_id.clone()),
            exact.blob_sha256.clone(),
        ),
        None => (
            toc_version(candidates, &gw.group.primary),
            VersionConfidence::Toc,
            None,
            None,
        ),
    };
    AddonRecord {
        r#ref: r.clone(),
        name: name.to_owned(),
        version,
        version_confidence,
        release_id,
        channel,
        pinned: false,
        reason: AddonReason::Explicit,
        required_by: Vec::new(),
        folders: gw
            .group
            .folders
            .iter()
            .filter(|f| !unloaded.contains(f.key()))
            .cloned()
            .collect(),
        state: AddonState::Ok,
        installed_at: jiff::Timestamp::now(),
        tx_id: TxId::new(),
        blob_sha256,
        revision: None,
        left_out: Vec::new(),
        any_flavor: false,
    }
}

/// A failed item decided before the engine: its error with its details.
fn failed_item(request: String, error: &Error) -> ItemResult {
    let mut item_error = ItemError::from(error);
    if !error.details.is_null() {
        item_error.details = Some(error.details.clone());
    }
    ItemResult {
        request,
        name: None,
        r#ref: None,
        status: ItemStatus::Failed,
        skip_reason: None,
        duplicate_of: None,
        reason: None,
        from_version: None,
        to_version: None,
        folders: None,
        required_by: None,
        deps: ItemDeps::default(),
        error: Some(item_error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::testing::TxScene;
    use crate::engine::witness::WitnessScope;
    use crate::error::{ErrorBody, UsageFix};
    use crate::model::TargetKey;

    fn argv(words: &[&str]) -> Vec<String> {
        words.iter().map(|word| (*word).to_owned()).collect()
    }

    #[test]
    fn the_report_refuses_if_match_and_its_fix_drops_it() {
        let token = format!("pc1:{}", "0".repeat(64));
        let inline = format!("--if-match={token}");
        for (case, report, has_token, words, expected) in [
            (
                "spaced",
                true,
                true,
                argv(&["omaddons", "adopt", "--if-match", &token, "-t", "retail"]),
                Some(argv(&["omaddons", "adopt", "-t", "retail"])),
            ),
            (
                "inline",
                true,
                true,
                argv(&["omaddons", "adopt", &inline]),
                Some(argv(&["omaddons", "adopt"])),
            ),
            (
                "dry run kept",
                true,
                true,
                argv(&["omaddons", "adopt", "--dry-run", "--if-match", &token]),
                Some(argv(&["omaddons", "adopt", "--dry-run"])),
            ),
            (
                "json dropped",
                true,
                true,
                argv(&["omaddons", "adopt", "--if-match", &token, "--json"]),
                Some(argv(&["omaddons", "adopt"])),
            ),
            (
                "report without the flag",
                true,
                false,
                argv(&["omaddons", "adopt"]),
                None,
            ),
            (
                "adopting with the flag",
                false,
                true,
                argv(&["omaddons", "adopt", "--apply", "--if-match", &token]),
                None,
            ),
        ] {
            let actual = report_takes_no_token(report, has_token.then_some(token.as_str()), &words);
            if let Some(expected) = expected {
                let error = actual.unwrap_err();
                assert_eq!(error.usage_fix, UsageFix::UserVerb, "{case}: {words:?}");
                let body = ErrorBody::from(error);
                assert_eq!(body.code, ErrorCode::Usage, "{case}: {words:?}, {body:?}");
                assert_eq!(body.fix_argv, Some(expected), "{case}: {words:?}, {body:?}");
                assert!(
                    body.message.contains("--if-match binds a run that adopts"),
                    "{case}: {words:?}, {body:?}"
                );
            } else {
                assert!(actual.is_ok(), "{case}: {words:?}, {actual:?}");
            }
        }
    }

    fn work(discovered: &DiscoveredTarget) -> TargetWork<'_> {
        TargetWork {
            discovered,
            unmanaged: 0,
            entries: Vec::new(),
            candidates: Vec::new(),
            groups: Vec::new(),
            leftovers: Vec::new(),
            failed: Vec::new(),
            acts: Vec::new(),
        }
    }

    #[test]
    fn the_noop_scope_is_every_examined_target_with_its_generation() {
        let scene = TxScene::new();
        let key = TargetKey::new();
        let mut registered = scene.discovered.clone();
        registered.target.key = Some(key);
        let mut unregistered = scene.discovered.clone();
        unregistered.target.id = "era".into();
        let mut record = TargetRecord::empty(key);
        record.generation = 5;
        let records = BTreeMap::from([("retail".into(), record)]);
        for (case, works, expected) in [
            (
                "registered then unregistered",
                vec![work(&registered), work(&unregistered)],
                vec![("retail", Some(key), 5), ("era", None, 0)],
            ),
            (
                "unregistered then registered",
                vec![work(&unregistered), work(&registered)],
                vec![("era", None, 0), ("retail", Some(key), 5)],
            ),
        ] {
            let actual = noop_scope(&works, &records);
            let actual: Vec<_> = actual
                .iter()
                .map(|i| (i.target.id.as_str(), i.target.key, i.generation))
                .collect();
            assert_eq!(actual, expected, "{case}: records={records:?}");
        }
    }

    #[test]
    fn an_adoption_previewed_twice_gives_one_token_and_an_edit_moves_observed() {
        let scene = TxScene::new();
        let bar = scene.client.addons().join("Bar");
        crate::engine::testing::folder(&bar, "unmanaged Bar");
        let core = bar.join("core.lua");
        std::fs::write(&core, "-- old\n").unwrap();
        let preview = || {
            let done = scene
                .run_bound(
                    vec![scene.adopt("Bar")],
                    Some(TxKind::Adopt),
                    TxKind::Adopt,
                    true,
                    Some(WitnessScope::default()),
                    None,
                )
                .unwrap();
            assert!(done.failures.is_empty(), "Bar: {:?}", done.failures);
            precondition::of_plan(
                "adopt",
                &done.plan,
                &done.plan.witnesses,
                &PlanExtras::default(),
            )
        };
        let first = preview();
        let second = preview();
        assert_eq!(
            first.token(),
            second.token(),
            "Bar unchanged: first={:?}, second={:?}",
            first.parts(),
            second.parts()
        );
        std::fs::write(&core, "-- new\n").unwrap();
        let edited = preview();
        let before = first.parts();
        let after = edited.parts();
        let differs = std::array::from_fn::<_, 4, _>(|i| before[i] != after[i]);
        assert_eq!(
            differs,
            [false, false, false, true],
            "Bar/core.lua same-size edit: before={before:?}, after={after:?}"
        );
    }
}
