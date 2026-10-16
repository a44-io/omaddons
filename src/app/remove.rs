//! `omaddons remove <addon>... [-t T]`: takes managed addons out of one target as an
//! undoable transaction (kind `remove`). Nothing is deleted: each folder moves into the
//! transaction's snapshot, so `omaddons undo` puts it back. SavedVariables stay.
//!
//! A name matches a managed addon by record name (any case), by canonical ref, or by a
//! folder it owns. Without `-t` every name must resolve to the same single target;
//! otherwise `target_ambiguous` with one exact command per candidate. Protected,
//! linked and unmanaged folders are never removable: naming one is `not_managed`.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;

use schemars::JsonSchema;
use serde::Serialize;

use super::adopt::hints::{adopt_fixes, adopting};
use super::engine::Engine;
use super::history::TargetRef;
use super::install::all_failed;
use super::outcome::ItemError;
use super::world::World;
use crate::cli::Context;
use crate::discover::DiscoveredTarget;
use crate::discover::registry::{resolve_one, with_target};
use crate::engine::plan::RequestedChange;
use crate::engine::{TxOutcomes, TxRequest, TxTarget};
use crate::error::{CommandHint, Error, ErrorCode, Result, Suggestion, Warning};
use crate::inventory::{Depth, FolderClass, inventory};
use crate::model::{AddonRecord, FolderName, Ref, ReloadRequired, RunId, TxId, TxKind};
use crate::output::{Outcome, Style};
use crate::state::records::TargetRecord;

/// `remove`'s own arguments.
#[derive(Debug, Clone, Default, clap::Args)]
pub struct RemoveArgs {
    /// What to remove: an addon's name, its ref (gh:Owner/Repo) or a folder it owns
    #[arg(required = true, value_name = "ADDON")]
    pub addons: Vec<String>,
}

/// `remove`'s data.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct RemoveData {
    pub dry_run: bool,
    pub target: TargetRef,
    pub tx: Option<TxId>,
    /// In argument order.
    pub items: Vec<RemoveItem>,
    pub reload_required: ReloadRequired,
    /// `null` when no swap happened.
    pub atomic_swap: Option<bool>,
}

/// One requested addon.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct RemoveItem {
    /// The argument exactly as given.
    pub request: String,
    pub name: Option<String>,
    #[serde(rename = "ref")]
    pub r#ref: Option<Ref>,
    pub status: RemoveStatus,
    pub version: Option<String>,
    /// Folders moved into the snapshot, sorted by key.
    pub folders: Vec<FolderName>,
    pub error: Option<ItemError>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RemoveStatus {
    Removed,
    Failed,
}

/// `remove`'s outcome.
#[derive(Debug)]
pub struct RemoveOutcome {
    data: RemoveData,
    warnings: Vec<Warning>,
    commands: Vec<CommandHint>,
    /// The global lock's hold time, for the `-v` log line.
    pub lock_held_ms: Option<u64>,
}

impl Outcome for RemoveOutcome {
    type Data = RemoveData;

    fn command(&self) -> &'static str {
        "remove"
    }

    fn data(&self) -> &RemoveData {
        &self.data
    }

    fn warnings(&self) -> &[Warning] {
        &self.warnings
    }

    fn commands(&self) -> &[CommandHint] {
        &self.commands
    }

    fn partial(&self) -> bool {
        self.data
            .items
            .iter()
            .any(|i| i.status == RemoveStatus::Failed)
    }

    fn render_human(&self, w: &mut dyn Write, _style: &Style) -> std::io::Result<()> {
        let d = &self.data;
        let id = &d.target.id;
        for item in &d.items {
            let name = item.name.as_deref().unwrap_or(&item.request);
            match item.status {
                RemoveStatus::Failed => {
                    let (code, message) = item
                        .error
                        .as_ref()
                        .map_or(("", ""), |e| (e.code.wire(), e.message.as_str()));
                    writeln!(w, "{id}: failed {name}: {code}: {message}")?;
                }
                RemoveStatus::Removed => {
                    let verb = if d.dry_run { "would remove" } else { "removed" };
                    let n = item.folders.len();
                    let folders = if n == 1 {
                        "1 folder".to_owned()
                    } else {
                        format!("{n} folders")
                    };
                    let version = item.version.as_deref().unwrap_or("?");
                    writeln!(w, "{id}: {verb} {name} {version} ({folders})")?;
                }
            }
        }
        let mut tail = Vec::new();
        if let Some(tx) = d.tx {
            tail.push(format!("tx {tx}"));
        }
        match d.reload_required {
            ReloadRequired::Reload => tail.push("/reload in game".to_owned()),
            ReloadRequired::Restart => tail.push("restart the game client".to_owned()),
            ReloadRequired::None => {}
        }
        if let Some(tx) = d.tx {
            tail.push(format!("undo: omaddons undo {tx}"));
        }
        if !tail.is_empty() {
            writeln!(w, "{}", tail.join(" · "))?;
        }
        Ok(())
    }
}

/// One positional: the addon's record name per target id it is managed in, or why it
/// cannot be removed.
struct Request {
    request: String,
    found: std::result::Result<BTreeMap<String, String>, Error>,
}

/// Runs `remove`.
pub fn run(ctx: &Context, args: &RemoveArgs) -> Result<RemoveOutcome> {
    let world = World::load()?;
    let records = world.records()?;
    let targets: Vec<&DiscoveredTarget> = match &ctx.target {
        Some(selector) => vec![resolve_one(
            selector,
            &world.resolved.targets,
            &world.table,
            &ctx.argv,
        )?],
        None => world
            .resolved
            .targets
            .iter()
            .filter(|d| d.target.key.is_some())
            .collect(),
    };
    let requests: Vec<Request> = args
        .addons
        .iter()
        .map(|request| Request {
            request: request.clone(),
            found: find(&world, &records, &targets, request),
        })
        .collect();

    // One transaction on one target: every resolved name must agree.
    let hit: BTreeSet<&str> = requests
        .iter()
        .filter_map(|r| r.found.as_ref().ok())
        .flat_map(|found| found.keys().map(String::as_str))
        .collect();
    let target_id = match hit.len() {
        0 => {
            let errors: Vec<(String, Error)> = requests
                .iter()
                .filter_map(|r| {
                    r.found
                        .as_ref()
                        .err()
                        .map(|e| (r.request.clone(), e.clone()))
                })
                .collect();
            return Err(all_failed(&errors));
        }
        1 => hit.iter().next().copied().unwrap_or_default(),
        _ => return Err(spans(ctx, &hit)),
    };
    let target = targets
        .iter()
        .copied()
        .find(|d| d.target.id == target_id)
        .ok_or_else(|| Error::internal("a resolved target vanished"))?;

    let mut names: Vec<&str> = Vec::new();
    for found in requests.iter().filter_map(|r| r.found.as_ref().ok()) {
        if let Some(name) = found.get(target_id)
            && !names.iter().any(|n| n.eq_ignore_ascii_case(name))
        {
            names.push(name);
        }
    }
    let req = TxRequest {
        targets: vec![TxTarget {
            discovered: target.clone(),
            changes: names
                .iter()
                .map(|n| RequestedChange::Remove {
                    addon: (*n).to_owned(),
                })
                .collect(),
            default_kind: TxKind::Remove,
            dependency_locks: std::collections::BTreeMap::new(),
        }],
        force: false,
        while_running: ctx.while_running,
        dry_run: ctx.dry_run,
        reason: ctx.reason.clone(),
        kind: None,
        run: RunId::new(),
        no_deps: false,
    };
    // A remove fetches nothing.
    let done = Engine::new(ctx, &world, "remove")?.run(&req, None)?;
    let built: Vec<(RemoveItem, Option<Error>)> =
        requests.iter().map(|r| item(r, target_id, &done)).collect();
    let errors: Vec<(String, Error)> = built
        .iter()
        .filter_map(|(i, e)| e.clone().map(|e| (i.request.clone(), e)))
        .collect();
    if errors.len() == built.len() {
        return Err(all_failed(&errors));
    }

    let outcome = done.outcomes.first();
    let mut warnings = world.warnings.clone();
    for (request, error) in &errors {
        let mut warning = Warning::item(error);
        if !error.message.contains(request.as_str()) {
            warning.message = format!("{request}: {}", error.message);
        }
        warnings.push(warning);
    }
    if let Some(o) = outcome {
        warnings.extend(o.warnings.iter().cloned());
    }
    warnings.extend(done.warnings.iter().cloned());
    let mut unique: Vec<Warning> = Vec::new();
    for w in warnings {
        if !unique.contains(&w) {
            unique.push(w);
        }
    }
    let tx = outcome.and_then(|o| o.tx);
    let key = outcome
        .and_then(|o| o.key)
        .or(target.target.key)
        .ok_or_else(|| Error::internal("removing from an unregistered target"))?;
    Ok(RemoveOutcome {
        data: RemoveData {
            dry_run: ctx.dry_run,
            target: TargetRef {
                id: target.target.id.clone(),
                key,
            },
            tx,
            items: built.into_iter().map(|(i, _)| i).collect(),
            reload_required: outcome.map_or(ReloadRequired::None, |o| o.reload_required),
            atomic_swap: tx.and(outcome.map(|o| o.atomic_swap)),
        },
        warnings: unique,
        commands: tx
            .map(|tx| {
                vec![CommandHint::new(
                    "undo",
                    ["omaddons".to_owned(), "undo".to_owned(), tx.to_string()],
                    false,
                )]
            })
            .unwrap_or_default(),
        lock_held_ms: outcome.and_then(|o| o.lock_held_ms),
    })
}

/// The addon `request` names in each of `targets` (by target id), or why it cannot be
/// removed: `ref_ambiguous` when it names two addons in one target, `not_managed` when
/// it names none anywhere.
fn find(
    world: &World,
    records: &BTreeMap<String, TargetRecord>,
    targets: &[&DiscoveredTarget],
    request: &str,
) -> std::result::Result<BTreeMap<String, String>, Error> {
    let mut found = BTreeMap::new();
    for d in targets {
        let Some(record) = records.get(&d.target.id) else {
            continue;
        };
        match matches(record, request).as_slice() {
            [] => {}
            [one] => {
                found.insert(d.target.id.clone(), one.name.clone());
            }
            many => {
                let names: Vec<&str> = many.iter().map(|a| a.name.as_str()).collect();
                return Err(Error::new(
                    ErrorCode::RefAmbiguous,
                    format!(
                        "{request} names {} addons in {}: {}",
                        names.len(),
                        d.target.id,
                        names.join(", ")
                    ),
                    Suggestion::text(
                        "name the addon itself rather than a folder or ref they share",
                    ),
                )
                .details(serde_json::json!({ "candidates": names, "target": d.target.id })));
            }
        }
    }
    if found.is_empty() {
        return Err(not_managed(world, records, targets, request));
    }
    Ok(found)
}

/// The addons of `record` that `request` names: by record name (any case), by canonical
/// ref, or by the key of a folder it owns.
fn matches<'r>(record: &'r TargetRecord, request: &str) -> Vec<&'r AddonRecord> {
    let as_ref = request.parse::<Ref>().ok();
    let folder = request.to_ascii_lowercase();
    record
        .addons
        .values()
        .filter(|a| {
            a.name.eq_ignore_ascii_case(request)
                || as_ref.as_ref().is_some_and(|r| r.eq(&a.r#ref))
                || a.folders.iter().any(|f| f.key().eq(folder.as_str()))
        })
        .collect()
}

/// `not_managed`, saying what the named folder is when it is on disk.
fn not_managed(
    world: &World,
    records: &BTreeMap<String, TargetRecord>,
    targets: &[&DiscoveredTarget],
    request: &str,
) -> Error {
    let key = request.to_ascii_lowercase();
    let scope = match targets {
        [one] => format!("in {}", one.target.id),
        _ => "on any target".to_owned(),
    };
    let looked: Vec<&DiscoveredTarget> = match targets {
        [] => world.resolved.targets.iter().collect(),
        some => some.to_vec(),
    };
    for d in looked.iter().copied() {
        let record = records.get(&d.target.id);
        let Ok(inv) = inventory(&d.target, record, &world.table, Depth::Classes) else {
            continue;
        };
        let Some(entry) = inv.folders.iter().find(|e| e.key.eq(key.as_str())) else {
            continue;
        };
        let (message, fix) = match entry.class {
            FolderClass::Protected => (
                format!(
                    "{} is a Blizzard folder of the game client, which omaddons never changes",
                    entry.name
                ),
                Suggestion::text("this folder belongs to the game client and is never modified"),
            ),
            FolderClass::Linked => (
                format!("{} is a symlink, which omaddons never changes", entry.name),
                Suggestion::text("remove or rename the symlink yourself"),
            ),
            FolderClass::Unmanaged | FolderClass::Invalid => (
                format!(
                    "{} was not installed by omaddons, so omaddons does not remove it",
                    entry.name
                ),
                Suggestion::command(["omaddons", "list", "--unmanaged"]),
            ),
            FolderClass::Managed => continue,
        };
        let error = Error::new(ErrorCode::NotManaged, message, fix).details(serde_json::json!({
            "target": d.target.id,
            "folder": entry.name,
            "class": entry.class,
        }));
        // An unmanaged folder is adopted, then removable; an invalid one is only listed.
        return match entry.class {
            FolderClass::Unmanaged => {
                adopting(error, adopt_fixes(world, records, &looked, request))
            }
            _ => error,
        };
    }
    Error::new(
        ErrorCode::NotManaged,
        format!("{request} is not managed by omaddons {scope}"),
        Suggestion::command(["omaddons", "list"]),
    )
}

/// The names span several targets: one exact command per candidate.
fn spans(ctx: &Context, ids: &BTreeSet<&str>) -> Error {
    let ids: Vec<&str> = ids.iter().copied().collect();
    let first = ids.first().copied().unwrap_or_default();
    let mut error = Error::new(
        ErrorCode::TargetAmbiguous,
        format!(
            "the addons to remove are managed in {} targets ({}); one removal changes one target",
            ids.len(),
            ids.join(", ")
        ),
        Suggestion::command(with_target(&ctx.argv, first)),
    )
    .details(serde_json::json!({ "candidates": ids }));
    for id in ids.iter().skip(1) {
        error = error.alternative(Suggestion::command(with_target(&ctx.argv, id)));
    }
    error
}

/// One positional's result on `target_id`.
fn item(r: &Request, target_id: &str, done: &TxOutcomes) -> (RemoveItem, Option<Error>) {
    let failed = |name: Option<String>, error: &Error| {
        (
            RemoveItem {
                request: r.request.clone(),
                name,
                r#ref: None,
                status: RemoveStatus::Failed,
                version: None,
                folders: Vec::new(),
                error: Some(ItemError::from(error)),
            },
            Some(error.clone()),
        )
    };
    let name = match &r.found {
        Ok(found) => match found.get(target_id) {
            Some(name) => name,
            None => {
                let error = Error::new(
                    ErrorCode::NotManaged,
                    format!("{} is not managed in {target_id}", r.request),
                    Suggestion::command(["omaddons", "list", "-t", target_id]),
                );
                return failed(None, &error);
            }
        },
        Err(error) => return failed(None, error),
    };
    let same = |a: &str| a.eq_ignore_ascii_case(name);
    let failure = done
        .failures
        .iter()
        .find(|f| f.addon.as_deref().is_some_and(same) || (f.addon.is_none() && f.r#ref.is_none()))
        .and_then(|f| f.cause.as_ref());
    if let Some(error) = failure {
        return failed(Some(name.clone()), error);
    }
    let change = done
        .outcomes
        .first()
        .and_then(|o| o.changes.iter().find(|c| same(&c.addon)));
    let Some(change) = change else {
        // Removed by someone else between planning and the lock.
        let gone = Error::new(
            ErrorCode::NotManaged,
            format!("{name} is no longer managed in {target_id}"),
            Suggestion::command(["omaddons", "list", "-t", target_id]),
        );
        return failed(Some(name.clone()), &gone);
    };
    let mut folders = change.folders.removed.clone();
    folders.sort_by(|a, b| a.key().cmp(b.key()));
    (
        RemoveItem {
            request: r.request.clone(),
            name: Some(change.addon.clone()),
            r#ref: Some(change.r#ref.clone()),
            status: RemoveStatus::Removed,
            version: change.from_version.clone(),
            folders,
            error: None,
        },
        None,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AddonReason, AddonState, Channel, TargetKey, VersionConfidence};

    fn record(addons: &[(&str, &str, &[&str])]) -> TargetRecord {
        let mut record = TargetRecord::empty(TargetKey::new());
        for (name, r, folders) in addons {
            record.addons.insert(
                TargetRecord::addon_key(name),
                AddonRecord {
                    r#ref: r.parse().unwrap(),
                    name: (*name).into(),
                    version: "1".into(),
                    version_confidence: VersionConfidence::Release,
                    release_id: None,
                    channel: Channel::Stable,
                    pinned: false,
                    reason: AddonReason::Explicit,
                    required_by: vec![],
                    folders: folders
                        .iter()
                        .map(|f| FolderName::new(f).unwrap())
                        .collect(),
                    state: AddonState::Ok,
                    blob_sha256: None,
                    installed_at: "2026-10-14T00:00:00Z".parse().unwrap(),
                    tx_id: TxId::new(),
                },
            );
        }
        record
    }

    #[test]
    fn names_refs_and_owned_folders_match() {
        let r = record(&[
            (
                "DBM",
                "gh:DeadlyBossMods/DeadlyBossMods",
                &["DBM-Core", "DBM-GUI"],
            ),
            (
                "WeakAuras",
                "gh:WeakAuras/WeakAuras2",
                &["WeakAuras", "WeakAurasOptions"],
            ),
        ]);
        let names = |q: &str| {
            matches(&r, q)
                .iter()
                .map(|a| a.name.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(names("dbm"), ["DBM"], "record name, any case");
        assert_eq!(names("dbm-core"), ["DBM"], "an owned folder");
        assert_eq!(
            names("gh:WeakAuras/WeakAuras2"),
            ["WeakAuras"],
            "the canonical ref"
        );
        assert_eq!(names("weakaurasoptions"), ["WeakAuras"]);
        assert!(names("Questie").is_empty());

        // A folder one addon owns that names another: two candidates.
        let clash = record(&[("Foo", "gh:o/foo", &["Bar"]), ("Bar", "gh:o/bar", &["Baz"])]);
        assert_eq!(matches(&clash, "bar").len(), 2);
    }
}
