//! `backup restore <ID> --addon A` (M3-D27): each named addon resolved to the folders
//! whose SavedVariables come back, from the backup's own addon record first (what it owned
//! when the backup was taken), then planned, previewed and restored through the engine's
//! partial restore, in the whole restore's order (reserve, plan, refuse a running client,
//! preview or compare `--if-match`, begin with the exchange probed, restore, retention).

use super::backup::{
    AddonRestore, BackupRestoreArgs, BackupRestoreData, BackupRestoreOutcome, FoldersFrom,
    RestoreMode, RestoreSource, meta_digest, named_backup, retain_source, target_ref,
};
use super::precondition::{AddonsPart, RestoreBinding, check, of_restore_addons, preview_argv};
use super::world::World;
use crate::backup::{AddonProvenance, AddonsFile, BackupRef, EntryKind, Manifest};
use crate::cli::Context;
use crate::engine::Binding;
use crate::engine::restore::{self, Exchange};
use crate::engine::restore_addons::{self, AddonPlan, AddonSv, AddonsRequest};
use crate::error::{CommandHint, Error, ErrorCode, Result, Suggestion};
use crate::model::{FolderName, Ref, ReloadRequired, RunId, TxKind};
use crate::state::paths::ClientPaths;
use crate::state::records::{TargetRecord, load_record};
use crate::state::store;

const COMMAND: &str = "backup restore";

/// Alternatives a `not_found` offers at most.
const ALTERNATIVES_MAX: usize = 3;

/// One `--addon`, resolved (M3-D27 step 0).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Resolved {
    addon: String,
    folders: Vec<FolderName>,
    from: FoldersFrom,
    captured_version: Option<String>,
    installed_version: Option<String>,
    not_in_backup: Vec<FolderName>,
}

/// Runs `backup restore <ID> --addon A…`.
pub fn run(ctx: &Context, args: &BackupRestoreArgs) -> Result<BackupRestoreOutcome> {
    let world = World::load(ctx)?;
    let mut warnings = world.warnings.clone();
    let (d, backup) = named_backup(ctx, &world, &args.id, "a restore", &mut warnings)?;
    let reservation = restore::reserve(&backup)?;
    let record = match d.target.key {
        Some(key) => Some(load_record(&world.paths, &key)?.value),
        None => None,
    };
    let names = Names::read(&backup)?;
    let client = ClientPaths::new(&d.target.client_dir);
    let mut resolved: Vec<Resolved> = Vec::new();
    for request in &args.addon {
        let alt = |folder: &str| with_addon(&ctx.argv, request, folder);
        let one = resolve(request, &names, record.as_ref(), &alt, &d.target.id)
            .map_err(|error| private(error, &client, &backup))?;
        if !resolved
            .iter()
            .any(|r| r.addon.eq_ignore_ascii_case(&one.addon))
        {
            resolved.push(one);
        }
    }
    let sv: Vec<AddonSv> = resolved
        .iter()
        .map(|r| AddonSv {
            addon: r.addon.clone(),
            folders: r.folders.clone(),
        })
        .collect();
    let engine = super::engine::Engine::new(ctx, &world, COMMAND)?;
    let plan = restore_addons::plan(&world.paths, &d.target, &backup, &sv)?;
    engine.refuse_restore(&d.target, plan.files.len() as u64)?;
    if plan.files.is_empty() {
        return Err(nothing(&backup, &resolved, &d.target.id));
    }
    warnings.extend(restore_addons::out_of_scope_warnings(
        &d.target.id,
        plan.out_of_scope,
    ));
    let meta = backup
        .meta
        .clone()
        .ok_or_else(|| Error::internal("a restorable backup has its metadata"))?;
    let bound = of_restore_addons(&AddonsPart {
        key: d.target.key,
        generation: plan.generation,
        backup: backup.id,
        folders: sv
            .iter()
            .flat_map(|a| a.folders.iter().map(|f| f.as_str().to_owned()))
            .collect(),
        files: plan
            .files
            .iter()
            .map(|f| (f.live.as_str(), f.action.wire()))
            .collect(),
        backup_json: meta_digest(&backup),
        saved_variables: crate::engine::swap::sv_stamp(&client).to_string(),
    });
    let mut data = BackupRestoreData {
        dry_run: ctx.dry_run,
        target: target_ref(d),
        tx: None,
        kind: TxKind::Restore,
        mode: RestoreMode::Addons,
        backup: RestoreSource {
            id: backup.id,
            captured_at: meta.captured_at,
            kind: meta.kind,
            integrity: meta.integrity,
        },
        files: None,
        before_restore: None,
        reload_required: ReloadRequired::None,
        precondition: None,
        exchange: Exchange::Unverified,
        addons: Some(restored(&resolved, &plan)),
    };
    let outcome = |data, warnings, commands, lock_held_ms| BackupRestoreOutcome {
        data,
        warnings,
        commands,
        trigger: meta.trigger,
        baseline: meta.baseline,
        would_keep: false,
        lock_held_ms,
    };
    if ctx.dry_run {
        // The mount checks only: no probe, nothing written.
        let topology = restore::preflight(&client, &backup.wtf(), None)?;
        restore_addons::check_mounts(&client, &plan, topology.stage_mount, &backup.wtf())?;
        data.precondition = Some(bound.token());
        return Ok(outcome(data, warnings, Vec::new(), None));
    }
    let preview = preview_argv(&ctx.argv);
    check(ctx.if_match.as_deref(), &bound, &preview)?;
    let restore::Begun {
        tx,
        inflight,
        topology,
    } = restore::begin(&world.paths, &client, &backup)?;
    let binding = ctx.if_match.as_ref().map(|_| RestoreBinding { preview });
    let req = AddonsRequest {
        target: d,
        backup: &backup,
        reservation,
        topology,
        tx,
        inflight,
        plan: &plan,
        addons: &sv,
        reason: ctx.reason.clone(),
        run: RunId::new(),
    };
    let done = engine.restore_addons(req, binding.as_ref().map(|b| b as &dyn Binding))?;
    warnings.extend(done.warnings.iter().cloned());
    warnings.extend(retain_source(ctx, &world, d, done.key, backup.id));
    data.tx = Some(done.tx);
    data.target.key = Some(done.key);
    data.backup.integrity = done.integrity;
    data.exchange = Exchange::Verified;
    let tx = done.tx.to_string();
    let undo = CommandHint::new("undo", ["omaddons", "undo", tx.as_str()], false);
    Ok(outcome(data, warnings, vec![undo], Some(done.lock_held_ms)))
}

/// What a backup says of its addons: its addon record (`addons.json`; empty when it has
/// none, as an older backup may), and the folders whose SavedVariables files its manifest
/// holds, by lowercase name, spelled as the files are.
struct Names {
    captured: AddonsFile,
    folders: std::collections::BTreeMap<String, FolderName>,
}

impl Names {
    fn read(backup: &BackupRef) -> Result<Self> {
        let captured = store::load::<AddonsFile>(&backup.dir.join("addons.json"))
            .ok()
            .flatten()
            .map(|loaded| loaded.value)
            .unwrap_or_default();
        let manifest = store::load::<Manifest>(&backup.dir.join("manifest.json"))?
            .map(|loaded| loaded.value)
            .unwrap_or_default();
        let mut folders = std::collections::BTreeMap::new();
        for entry in manifest
            .entries
            .iter()
            .filter(|e| e.kind == EntryKind::File)
        {
            let mut parts = entry.path.rsplit('/');
            let (Some(name), Some(dir)) = (parts.next(), parts.next()) else {
                continue;
            };
            if !dir.eq_ignore_ascii_case("SavedVariables") {
                continue;
            }
            let lower = name.to_ascii_lowercase();
            let Some(stem) = [".lua.bak", ".lua"]
                .into_iter()
                .find_map(|suffix| lower.strip_suffix(suffix).map(str::len))
            else {
                continue;
            };
            let Some(folder) = name.get(..stem).and_then(|s| FolderName::new(s).ok()) else {
                continue;
            };
            if folder.key() == "savedvariables" {
                continue;
            }
            folders.entry(folder.key().to_owned()).or_insert(folder);
        }
        Ok(Self { captured, folders })
    }

    /// The backup's folder named `name` (any case): one it holds SavedVariables of, or one
    /// its addon record names (whose settings it then holds none of).
    fn folder(&self, name: &str) -> Option<FolderName> {
        let key = name.to_ascii_lowercase();
        self.folders.get(&key).cloned().or_else(|| {
            self.captured
                .addons
                .iter()
                .flat_map(|a| &a.folders)
                .find(|f| f.key() == key)
                .cloned()
        })
    }
}

/// The prefix that names one folder's settings only: `--addon folder:WeakAurasOptions`.
const FOLDER: &str = "folder:";

/// Resolves `request` (M3-D27): `folder:<F>`, that folder of the backup only; else an
/// addon of the backup's record by name, ref or folder (the folders it owned then); else an addon
/// managed on the target now by name, ref or folder (the folders it owns now); else a
/// folder whose SavedVariables the backup holds; else `not_found`. `alt` is the same
/// command with another `--addon` in place of `request`.
fn resolve(
    request: &str,
    names: &Names,
    record: Option<&TargetRecord>,
    alt: &dyn Fn(&str) -> Vec<String>,
    target_id: &str,
) -> Result<Resolved> {
    let prefixed = request
        .get(..FOLDER.len())
        .filter(|p| p.eq_ignore_ascii_case(FOLDER))
        .and_then(|_| request.get(FOLDER.len()..));
    if let Some(folder) = prefixed {
        return match names.folder(folder) {
            Some(folder) => Ok(one_folder(&folder, names, record)),
            None => Err(not_found(request, names, alt, target_id)),
        };
    }
    let as_ref = request.parse::<Ref>().ok();
    let folder = request.to_ascii_lowercase();
    let named: Vec<&AddonProvenance> = names
        .captured
        .addons
        .iter()
        .filter(|a| {
            a.name.eq_ignore_ascii_case(request)
                || as_ref
                    .as_ref()
                    .is_some_and(|r| a.source.r#ref.parse::<Ref>().ok().as_ref() == Some(r))
                || a.folders.iter().any(|f| f.key() == folder)
        })
        .collect();
    match named.as_slice() {
        [one] => return captured(one, record, alt, target_id),
        [] => {}
        many => {
            let candidates: Vec<&str> = many.iter().map(|a| a.name.as_str()).collect();
            return Err(ambiguous_names(request, target_id, &candidates, alt));
        }
    }
    if let Some(record) = record {
        match super::remove::matches(record, request).as_slice() {
            [one] => {
                return Ok(Resolved {
                    addon: one.name.clone(),
                    folders: one.folders.clone(),
                    from: FoldersFrom::Installed,
                    captured_version: None,
                    installed_version: Some(one.version.clone()),
                    not_in_backup: Vec::new(),
                });
            }
            [] => {}
            many => {
                let candidates: Vec<&str> = many.iter().map(|a| a.name.as_str()).collect();
                return Err(ambiguous_names(request, target_id, &candidates, alt));
            }
        }
    }
    match names.folders.get(&request.to_ascii_lowercase()) {
        Some(folder) => Ok(one_folder(folder, names, record)),
        None => Err(not_found(request, names, alt, target_id)),
    }
}

/// One folder of the backup, alone: its owner's versions then and now, when known.
fn one_folder(folder: &FolderName, names: &Names, record: Option<&TargetRecord>) -> Resolved {
    let then = names
        .captured
        .addons
        .iter()
        .find(|a| a.folders.iter().any(|f| f.key() == folder.key()));
    let now = record.and_then(|r| {
        r.addons
            .values()
            .find(|a| a.folders.iter().any(|f| f.key() == folder.key()))
    });
    Resolved {
        addon: folder.as_str().to_owned(),
        folders: vec![folder.clone()],
        from: FoldersFrom::Folder,
        captured_version: then.and_then(|a| a.version.clone()),
        installed_version: now.map(|a| a.version.clone()),
        not_in_backup: Vec::new(),
    }
}

/// An addon of the backup's record: the folders it owned then. One of them another
/// managed addon owns now is `ref_ambiguous`, never broadened (M3-D27), with each folder
/// alone (`folder:<F>`, which a folder named like the addon needs) as an alternative; one
/// it owns now that it did not then is left as it is (`not_in_backup`).
fn captured(
    one: &AddonProvenance,
    record: Option<&TargetRecord>,
    alt: &dyn Fn(&str) -> Vec<String>,
    target_id: &str,
) -> Result<Resolved> {
    let now = record.and_then(|r| r.addons.get(&TargetRecord::addon_key(&one.name)));
    let owner = |folder: &FolderName| {
        record.and_then(|r| {
            r.addons.values().find(|a| {
                !a.name.eq_ignore_ascii_case(&one.name)
                    && a.folders.iter().any(|f| f.key() == folder.key())
            })
        })
    };
    let taken: Vec<(&FolderName, &str)> = one
        .folders
        .iter()
        .filter_map(|f| owner(f).map(|a| (f, a.name.as_str())))
        .collect();
    if let Some((folder, other)) = taken.first() {
        let mut error = Error::new(
            ErrorCode::RefAmbiguous,
            format!(
                "{} was {}'s when the backup was taken and is {other}'s now; name the folders whose settings to restore",
                folder.as_str(),
                one.name
            ),
            Suggestion::command(["omaddons", "backup", "list", "-t", target_id, "-v"]),
        );
        for folder in &one.folders {
            let alone = format!("{FOLDER}{}", folder.as_str());
            error = error.alternative(Suggestion::command(alt(&alone)));
        }
        let folders: Vec<&str> = one.folders.iter().map(FolderName::as_str).collect();
        let owned: Vec<serde_json::Value> = taken
            .iter()
            .map(|(f, a)| serde_json::json!({ "folder": f.as_str(), "owner": a }))
            .collect();
        return Err(error.details(serde_json::json!({
            "addon": one.name,
            "candidates": folders,
            "owned_now": owned,
            "target": target_id,
        })));
    }
    let not_in_backup = now
        .map(|a| {
            a.folders
                .iter()
                .filter(|f| !one.folders.iter().any(|g| g.key() == f.key()))
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    Ok(Resolved {
        addon: one.name.clone(),
        folders: one.folders.clone(),
        from: FoldersFrom::Backup,
        captured_version: one.version.clone(),
        installed_version: now.map(|a| a.version.clone()),
        not_in_backup,
    })
}

/// `ref_ambiguous`: `request` names several addons; each is an alternative.
fn ambiguous_names(
    request: &str,
    target_id: &str,
    candidates: &[&str],
    alt: &dyn Fn(&str) -> Vec<String>,
) -> Error {
    let mut error = Error::new(
        ErrorCode::RefAmbiguous,
        format!(
            "{request} names {} addons: {}; name the addon itself rather than a folder or ref they share",
            candidates.len(),
            candidates.join(", ")
        ),
        Suggestion::command(["omaddons", "backup", "list", "-t", target_id, "-v"]),
    );
    for name in candidates {
        error = error.alternative(Suggestion::command(alt(name)));
    }
    error.details(serde_json::json!({ "candidates": candidates, "target": target_id }))
}

/// `not_found`: no addon and no SavedVariables folder of the backup is `request`. The
/// alternatives are up to [`ALTERNATIVES_MAX`] folders it has, those whose name holds
/// the request first.
fn not_found(
    request: &str,
    names: &Names,
    alt: &dyn Fn(&str) -> Vec<String>,
    target_id: &str,
) -> Error {
    let wanted = request.to_ascii_lowercase();
    let mut folders: Vec<&FolderName> = names.folders.values().collect();
    folders.sort_by_key(|f| (!f.key().contains(wanted.as_str()), f.key().to_owned()));
    let mut error = Error::new(
        ErrorCode::NotFound,
        format!("the backup has no addon and no SavedVariables folder named {request}"),
        Suggestion::command(["omaddons", "backup", "list", "-t", target_id, "-v"]),
    );
    for folder in folders.iter().take(ALTERNATIVES_MAX) {
        error = error.alternative(Suggestion::command(alt(folder.as_str())));
    }
    error.details(serde_json::json!({ "addon": request, "target": target_id }))
}

/// `not_found`: the backup and the live client hold no SavedVariables of the addons named.
fn nothing(backup: &BackupRef, resolved: &[Resolved], target_id: &str) -> Error {
    let names: Vec<&str> = resolved.iter().map(|r| r.addon.as_str()).collect();
    Error::new(
        ErrorCode::NotFound,
        format!(
            "neither backup {} nor {target_id} holds SavedVariables of {}",
            backup.id,
            names.join(", ")
        ),
        Suggestion::command(["omaddons", "backup", "list", "-t", target_id, "-v"]),
    )
    .details(serde_json::json!({ "addons": names, "target": target_id }))
}

/// `error` with every account, realm and character name the client and the backup hold
/// replaced in its message and details: a requested name can be anything. Its argvs stay
/// runnable.
fn private(mut error: Error, client: &ClientPaths, backup: &BackupRef) -> Error {
    use crate::privacy::WtfPathRedactor;
    let live = WtfPathRedactor::for_client(client);
    let kept = WtfPathRedactor::for_client(&ClientPaths::new(&backup.dir));
    let text = |s: &str| kept.text(&live.text(s));
    error.message = text(&error.message);
    let details = text(&error.details.to_string());
    error.details = serde_json::from_str(&details).unwrap_or_else(|_| serde_json::json!({}));
    error
}

/// `argv` (without presentation flags) with the `--addon` that gave `request` giving
/// `folder` instead.
fn with_addon(argv: &[String], request: &str, folder: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(argv.len());
    let mut replaced = false;
    let mut args = argv.iter().peekable();
    while let Some(arg) = args.next() {
        if !replaced && arg == "--addon" && args.peek().is_some_and(|v| *v == request) {
            args.next();
            out.extend(["--addon".to_owned(), folder.to_owned()]);
            replaced = true;
        } else if !replaced && arg.strip_prefix("--addon=") == Some(request) {
            out.extend(["--addon".to_owned(), folder.to_owned()]);
            replaced = true;
        } else {
            out.push(arg.clone());
        }
    }
    crate::cli::fix_argv(&out)
}

/// The data's `addons`: each resolved addon with its files' counts.
fn restored(resolved: &[Resolved], plan: &AddonPlan) -> Vec<AddonRestore> {
    resolved
        .iter()
        .zip(&plan.counts)
        .map(|(r, files)| AddonRestore {
            addon: r.addon.clone(),
            folders: r.folders.clone(),
            folders_from: r.from,
            captured_version: r.captured_version.clone(),
            installed_version: r.installed_version.clone(),
            not_in_backup: r.not_in_backup.clone(),
            files: *files,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backup::SourceNote;
    use crate::model::{
        AddonReason, AddonRecord, AddonState, Channel, TargetKey, TxId, VersionConfidence,
    };

    fn argv(args: &[&str]) -> Vec<String> {
        args.iter().map(|a| (*a).to_owned()).collect()
    }

    fn folders(names: &[&str]) -> Vec<FolderName> {
        names.iter().map(|f| FolderName::new(f).unwrap()).collect()
    }

    /// A backup's names: `captured` (name, ref, version, folders) in its addon record,
    /// `files` the folders whose SavedVariables it holds.
    fn names(captured: &[(&str, &str, &str, &[&str])], files: &[&str]) -> Names {
        Names {
            captured: AddonsFile {
                addons: captured
                    .iter()
                    .map(|(name, r, version, f)| AddonProvenance {
                        name: (*name).to_owned(),
                        folders: folders(f),
                        version: Some((*version).to_owned()),
                        source: SourceNote {
                            kind: "github".to_owned(),
                            r#ref: (*r).to_owned(),
                            release_id: None,
                        },
                    })
                    .collect(),
            },
            folders: folders(files)
                .into_iter()
                .map(|f| (f.key().to_owned(), f))
                .collect(),
        }
    }

    /// A target record of `addons` (name, ref, version, folders).
    fn record(addons: &[(&str, &str, &str, &[&str])]) -> TargetRecord {
        let mut record = TargetRecord::empty(TargetKey::new());
        for (name, r, version, f) in addons {
            record.addons.insert(
                TargetRecord::addon_key(name),
                AddonRecord {
                    r#ref: r.parse().unwrap(),
                    name: (*name).into(),
                    version: (*version).into(),
                    version_confidence: VersionConfidence::Release,
                    release_id: None,
                    channel: Channel::Stable,
                    pinned: false,
                    reason: AddonReason::Explicit,
                    required_by: vec![],
                    folders: folders(f),
                    state: AddonState::Ok,
                    blob_sha256: None,
                    revision: None,
                    left_out: Vec::new(),
                    any_flavor: false,
                    installed_at: "2026-10-14T00:00:00Z".parse().unwrap(),
                    tx_id: TxId::new(),
                },
            );
        }
        record
    }

    fn resolved(request: &str, names: &Names, record: Option<&TargetRecord>) -> Result<Resolved> {
        let alt = |folder: &str| argv(&["omaddons", "backup", "restore", "X", "--addon", folder]);
        resolve(request, names, record, &alt, "retail")
    }

    #[test]
    fn an_addon_resolves_to_what_it_owned_when_the_backup_was_taken() {
        let wa = (
            "WeakAuras",
            "gh:o/WeakAuras",
            "5.19.2",
            &["WeakAuras", "WeakAurasOptions"][..],
        );
        let backup = names(&[wa], &["WeakAuras", "Foo"]);
        // By name (any case), ref or folder; installed now with a new folder.
        let now = record(&[(
            "WeakAuras",
            "gh:o/WeakAuras",
            "5.20.0",
            &["WeakAuras", "WeakAurasConfig"],
        )]);
        for request in ["weakauras", "gh:o/WeakAuras", "WEAKAURASOPTIONS"] {
            let r = resolved(request, &backup, Some(&now)).unwrap();
            assert_eq!(
                (
                    r.addon.as_str(),
                    r.from,
                    r.folders.clone(),
                    r.captured_version.as_deref(),
                    r.installed_version.as_deref(),
                    r.not_in_backup.clone()
                ),
                (
                    "WeakAuras",
                    FoldersFrom::Backup,
                    folders(&["WeakAuras", "WeakAurasOptions"]),
                    Some("5.19.2"),
                    Some("5.20.0"),
                    folders(&["WeakAurasConfig"])
                ),
                "{request}"
            );
        }
        // Removed since: still the backup's, nothing installed.
        let r = resolved("WeakAuras", &backup, None).unwrap();
        assert_eq!((r.from, r.installed_version), (FoldersFrom::Backup, None));
    }

    #[test]
    fn otherwise_today_s_record_then_a_folder_of_the_backup() {
        let backup = names(&[], &["Foo", "Bar"]);
        let now = record(&[("Plater", "gh:o/Plater", "1.2", &["Plater"])]);
        let r = resolved("plater", &backup, Some(&now)).unwrap();
        assert_eq!(
            (r.from, r.folders, r.installed_version.as_deref()),
            (FoldersFrom::Installed, folders(&["Plater"]), Some("1.2"))
        );
        let r = resolved("foo", &backup, Some(&now)).unwrap();
        assert_eq!(
            (r.addon.as_str(), r.from, r.folders),
            ("Foo", FoldersFrom::Folder, folders(&["Foo"]))
        );
        // `folder:` names one folder alone, even one named like an addon.
        let wa = (
            "WeakAuras",
            "gh:o/WeakAuras",
            "5",
            &["WeakAuras", "WeakAurasOptions"][..],
        );
        let backup = names(&[wa], &["WeakAuras"]);
        let r = resolved("folder:weakauras", &backup, None).unwrap();
        assert_eq!(
            (r.from, r.folders),
            (FoldersFrom::Folder, folders(&["WeakAuras"]))
        );
        // A folder its record names, whose settings it holds none of.
        let r = resolved("FOLDER:WeakAurasOptions", &backup, None).unwrap();
        assert_eq!(r.folders, folders(&["WeakAurasOptions"]));
    }

    #[test]
    fn a_folder_another_addon_owns_now_is_ambiguous_and_an_unknown_name_not_found() {
        let wa = (
            "WeakAuras",
            "gh:o/WeakAuras",
            "5",
            &["WeakAuras", "WeakAurasOptions"][..],
        );
        let backup = names(
            &[wa],
            &["WeakAuras", "Foo", "Bar", "Baz", "WeakAurasOptions"],
        );
        let now = record(&[
            ("WeakAuras", "gh:o/WeakAuras", "6", &["WeakAuras"]),
            ("Opts", "gh:o/Opts", "1", &["WeakAurasOptions"]),
        ]);
        let err = resolved("WeakAuras", &backup, Some(&now)).unwrap_err();
        assert_eq!(err.code, ErrorCode::RefAmbiguous);
        let alternatives: Vec<&str> = err
            .alternatives
            .iter()
            .map(|a| a.argv().unwrap().last().unwrap().as_str())
            .collect();
        assert_eq!(
            alternatives,
            ["folder:WeakAuras", "folder:WeakAurasOptions"]
        );
        assert_eq!(err.details["owned_now"][0]["owner"], "Opts");

        let err = resolved("Nothing", &backup, None).unwrap_err();
        assert_eq!(err.code, ErrorCode::NotFound);
        assert_eq!(err.alternatives.len(), ALTERNATIVES_MAX);
        // Those whose name holds the request first.
        let err = resolved("weak", &backup, None).unwrap_err();
        let first = err.alternatives[0].argv().unwrap().last().unwrap().clone();
        assert!(first.to_ascii_lowercase().contains("weak"), "{first}");
    }

    #[test]
    fn an_alternative_names_the_folder_in_place_of_the_request() {
        let given = argv(&[
            "omaddons", "backup", "restore", "01M6K2VA", "-t", "retail", "--addon", "Wa",
            "--addon", "Plater", "--json",
        ]);
        assert_eq!(
            with_addon(&given, "Wa", "WeakAuras"),
            argv(&[
                "omaddons",
                "backup",
                "restore",
                "01M6K2VA",
                "-t",
                "retail",
                "--addon",
                "WeakAuras",
                "--addon",
                "Plater",
            ])
        );
        let equals = argv(&[
            "omaddons",
            "backup",
            "restore",
            "--addon=Wa",
            "--",
            "01M6K2VA",
        ]);
        assert_eq!(
            with_addon(&equals, "Wa", "WeakAuras"),
            argv(&[
                "omaddons",
                "backup",
                "restore",
                "--addon",
                "WeakAuras",
                "--",
                "01M6K2VA"
            ])
        );
    }
}
