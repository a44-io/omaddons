//! `omaddons sync [<file>] [-t T] [--strict] [--no-deps]`: makes each client match the
//! desired-state file (`<config>/omaddons/addons.toml` unless named), idempotently
//! (vision D32).
//!
//! Each selected target takes its family's section (live targets only) and its own
//! `[target."<id>"]` section, each entry resolved as `install` reads its arguments. A
//! missing addon is installed; a pinned entry at another version installs that version
//! pinned; an entry on another channel switches it; a pin that differs is a records-only
//! edit. An installed addon is never moved to a newer release (M2-D20: `update` does
//! that), so a second run changes nothing and asks no source anything. Each target with
//! changes commits one transaction of kind `sync`, all under one run, so `omaddons undo`
//! reverts the whole sync.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde_json::json;

use super::engine::{Engine, recover_waiting};
use super::outcome::{
    ItemDeps, ItemError, ItemRequest, ItemResult, ItemStatus, TargetRef, TargetResult, TargetsData,
    TargetsOutcome, sort_keeping_dependencies,
};
use super::resolve_args::{catalog_name, resolve_entry};
use super::world::World;
use crate::cli::Context;
use crate::discover::DiscoveredTarget;
use crate::engine::edit::RecordEdit;
use crate::engine::plan::{AnyFlavorIntent, PinIntent, RequestedChange};
use crate::engine::{TxOutcomes, TxRequest, TxTarget};
use crate::error::{CommandHint, Error, ErrorCode, Result, Suggestion, Warning, WarningCode};
use crate::model::{AddonReason, AddonRecord, Ref, RunId, Target, TxId, TxKind};
use crate::output::{Outcome, Style};
use crate::politeness::SOURCE_RECHECK_MIN_INTERVAL;
use crate::resolve::cache::Freshness;
use crate::resolve::catalog::Catalog;
use crate::rules::TargetKind;
use crate::sources::changelog::same_version;
use crate::state::desired::{self, DesiredState, Entry};
use crate::state::records::TargetRecord;

/// `sync`'s own arguments.
#[derive(Debug, Clone, Default, clap::Args)]
pub struct SyncArgs {
    /// The desired-state file (default: addons.toml in omaddons' config directory)
    #[arg(value_name = "FILE")]
    pub file: Option<PathBuf>,
    /// Exit 6 when only some of the addons sync
    #[arg(long)]
    pub strict: bool,
    /// Install only the listed addons, not the ones their TOCs require
    #[arg(long)]
    pub no_deps: bool,
    /// Install the versions the file records, from the blob cache when cached
    #[arg(long)]
    pub locked: bool,
    /// Also remove managed addons the file does not list (never one a listed addon
    /// requires or a lock names; refused while a target section names no known client)
    #[arg(long)]
    pub prune: bool,
}

/// `sync`'s outcome: the shared multi-target outcome, with `--strict`.
#[derive(Debug)]
pub struct SyncOutcome {
    inner: TargetsOutcome,
    strict: bool,
    /// Each committed transaction's global lock hold time, for `-v` log lines.
    pub locks: Vec<(TxId, u64)>,
    /// For `-v` log lines.
    pub notes: Vec<String>,
}

impl Outcome for SyncOutcome {
    type Data = TargetsData;

    fn command(&self) -> &'static str {
        "sync"
    }

    fn data(&self) -> &TargetsData {
        &self.inner.data
    }

    fn warnings(&self) -> &[Warning] {
        &self.inner.warnings
    }

    fn commands(&self) -> &[CommandHint] {
        &self.inner.commands
    }

    fn partial(&self) -> bool {
        self.inner.data.partial()
    }

    fn assertion_failure(&self) -> Option<Error> {
        let failed = self.inner.data.summary.failed;
        (self.strict && self.partial()).then(|| {
            let noun = if failed == 1 { "addon" } else { "addons" };
            Error::new(
                ErrorCode::CheckFailed,
                format!("{failed} {noun} did not sync (--strict)"),
                Suggestion::text("see data.targets[].items[].error for each failed addon"),
            )
        })
    }

    fn render_human(&self, w: &mut dyn std::io::Write, style: &Style) -> std::io::Result<()> {
        self.inner.render_human(w, style)
    }
}

/// One resolved entry a target requests (locks are not requests).
#[derive(Debug, Clone)]
pub struct Want<'s> {
    pub entry: &'s Entry,
    pub r#ref: Ref,
    /// The name a new install takes: the catalog's for a catalog ref, else the entry's
    /// `name` when it also gives a `ref`; else the archive decides.
    pub name: Option<String>,
}

/// What `sync` does about one entry on one target.
#[derive(Debug, Clone)]
pub struct DiffItem<'s, 'r> {
    pub entry: &'s Entry,
    /// The installed addon with the entry's ref, if any.
    pub installed: Option<&'r AddonRecord>,
    /// `None`: it matches already.
    pub change: Option<RequestedChange>,
}

/// The changes that make `record` match `wants`, one per entry at most (first matching
/// row wins), never moving an installed addon to a newer release by itself:
///
/// | situation | change | under `locked` |
/// |---|---|---|
/// | not installed | install (its `version` only when pinned) | its `version` whenever it has one |
/// | pinned at another `version` | install that version, pinned | the same |
/// | at another `version` | nothing | install that version, its pin as the entry says |
/// | another `channel` | install from that channel (a records-only switch when its newest release is installed) | at its `version` |
/// | only the pin differs | records-only `SetPinned` | the same |
/// | otherwise | nothing | nothing |
///
/// A lock never pins: an unpinned entry installs its version unpinned. A locked install
/// carries the entry's `sha256`, so its archive may come from the blob cache, and its
/// `revision` (WowAce), so it is that tree; the same version at another revision is
/// "another `version`".
pub fn diff<'s, 'r>(
    wants: &[Want<'s>],
    record: Option<&'r TargetRecord>,
    locked: bool,
) -> Vec<DiffItem<'s, 'r>> {
    wants
        .iter()
        .map(|w| {
            let e = w.entry;
            let installed = record.and_then(|r| {
                r.addons
                    .values()
                    .find(|a| a.r#ref.identity() == w.r#ref.identity())
            });
            // The entry's `any_flavor` against the record: set or cleared to match.
            let any_flavor = match installed {
                Some(a) if a.any_flavor == e.any_flavor => AnyFlavorIntent::Keep,
                _ if e.any_flavor => AnyFlavorIntent::Set,
                Some(_) => AnyFlavorIntent::Clear,
                None => AnyFlavorIntent::Keep,
            };
            let install = |r#ref: Ref, name: Option<String>, version: Option<String>, pin| {
                let blob_sha256 = e.sha256.clone().filter(|_| locked && version.is_some());
                // The recorded revision goes with the recorded version.
                let revision = e.revision.filter(|_| version.is_some());
                Some(RequestedChange::Install {
                    r#ref,
                    channel: e.channel,
                    channel_explicit: true,
                    version,
                    file: None,
                    name,
                    pin,
                    reason: AddonReason::Explicit,
                    reinstall: false,
                    blob_sha256,
                    asset: None,
                    revision,
                    any_flavor,
                })
            };
            // The version an install takes: pinned ones always, every one when locked.
            let pinned_version = || e.version.clone().filter(|_| e.pinned || locked);
            let change = match installed {
                None => {
                    let pin = if e.pinned {
                        PinIntent::Pin
                    } else {
                        PinIntent::Keep
                    };
                    install(w.r#ref.clone(), w.name.clone(), pinned_version(), pin)
                }
                Some(a) => {
                    let again = || (a.r#ref.clone(), Some(a.name.clone()));
                    let pin = if e.pinned == a.pinned {
                        PinIntent::Keep
                    } else if e.pinned {
                        PinIntent::Pin
                    } else {
                        PinIntent::Unpin
                    };
                    // At another version, or (WowAce) the same one at another revision.
                    // A version names the release as `install --version` reads it: a
                    // leading `v` aside (`1.2.3` finds `v1.2.3`), a sha by its prefix.
                    let moved = e.revision.is_some_and(|r| a.revision != Some(r));
                    let elsewhere = e
                        .version
                        .as_ref()
                        .is_some_and(|v| !same_version(v, &a.version) || moved);
                    if e.pinned && elsewhere {
                        let (r, name) = again();
                        install(r, name, e.version.clone(), PinIntent::Pin)
                    } else if locked && elsewhere {
                        let (r, name) = again();
                        install(r, name, e.version.clone(), pin)
                    } else if e.channel != a.channel {
                        let (r, name) = again();
                        install(r, name, pinned_version(), pin)
                    } else if e.pinned != a.pinned && any_flavor != AnyFlavorIntent::Keep {
                        // Both settings at once: an install that keeps its release.
                        let (r, name) = again();
                        install(r, name, None, pin)
                    } else if e.pinned != a.pinned {
                        Some(RequestedChange::Edit {
                            addon: a.name.clone(),
                            edit: RecordEdit::SetPinned(e.pinned),
                        })
                    } else if any_flavor != AnyFlavorIntent::Keep {
                        Some(RequestedChange::Edit {
                            addon: a.name.clone(),
                            edit: RecordEdit::SetAnyFlavor(e.any_flavor),
                        })
                    } else {
                        None
                    }
                }
            };
            DiffItem {
                entry: e,
                installed,
                change,
            }
        })
        .collect()
}

/// The records `--prune` keeps on a target (by record key): those whose ref one of
/// `refs` names (desired entries, locks, and every ref a failed entry may mean), then,
/// to a fixpoint, every record a kept one requires, whatever its `reason`.
pub fn keep_set(record: &TargetRecord, refs: &[Ref]) -> BTreeSet<String> {
    let named: BTreeSet<String> = refs.iter().map(Ref::identity).collect();
    let mut keep: BTreeSet<String> = record
        .addons
        .iter()
        .filter(|(_, a)| named.contains(&a.r#ref.identity()))
        .map(|(key, _)| key.clone())
        .collect();
    loop {
        let required: Vec<String> = record
            .addons
            .iter()
            .filter(|(key, _)| !keep.contains(*key))
            .filter(|(_, a)| {
                a.required_by
                    .iter()
                    .any(|by| keep.contains(&TargetRecord::addon_key(by)))
            })
            .map(|(key, _)| key.clone())
            .collect();
        if required.is_empty() {
            return keep;
        }
        keep.extend(required);
    }
}

/// `--prune` refuses a file with a `[target."<id>"]` section naming no known client: a
/// typo there would leave that client's addons out of its desired set.
fn unknown_section(state: &DesiredState, known: &[&str], path: &Path) -> Option<Error> {
    let (id, section) = state
        .targets
        .iter()
        .find(|(id, _)| !known.iter().any(|k| k.eq_ignore_ascii_case(id)))?;
    Some(
        Error::new(
            ErrorCode::DesiredStateInvalid,
            format!(
                "{} line {}: [target.\"{id}\"] names no known client, so --prune is refused (it could remove what that section meant to keep); sync without --prune is allowed",
                path.display(),
                section.line
            ),
            Suggestion::command(["omaddons", "targets"]),
        )
        .details(json!({
            "path": path.display().to_string(),
            "line": section.line,
            "key": format!("target.{id}"),
            "reason": "unknown_target_section",
        })),
    )
}

/// Every ref a `ref_ambiguous` error lists.
fn ambiguous_refs(error: &Error) -> Vec<Ref> {
    if error.code != ErrorCode::RefAmbiguous {
        return Vec::new();
    }
    error.details["candidates"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| c["ref"].as_str()?.parse().ok())
        .collect()
}

/// The positions of entries that resolve to a ref another entry resolves to: they may
/// disagree, so neither is synced.
fn resolved_duplicates(wants: &[Want<'_>]) -> BTreeMap<usize, usize> {
    let mut out = BTreeMap::new();
    for (i, a) in wants.iter().enumerate() {
        if let Some((j, _)) = wants
            .iter()
            .enumerate()
            .find(|(j, b)| *j != i && b.r#ref.identity() == a.r#ref.identity())
        {
            out.insert(i, j);
        }
    }
    out
}

/// What an item is called: the entry's name, else its ref.
fn label(entry: &Entry) -> String {
    entry
        .name
        .clone()
        .or_else(|| entry.r#ref.clone())
        .unwrap_or_default()
}

/// Whether `target` has a section in `state`: its family's when it is live, or its own.
fn has_section(state: &DesiredState, target: &Target) -> bool {
    (target.kind == TargetKind::Live && state.families.contains_key(&target.family))
        || state
            .targets
            .keys()
            .any(|id| id.eq_ignore_ascii_case(&target.id))
}

fn target_ref(d: &DiscoveredTarget) -> TargetRef {
    TargetRef {
        id: d.target.id.clone(),
        key: d.target.key,
        mode: None,
    }
}

/// An item that failed before the engine.
fn failed_item(request: String, r: Option<Ref>, error: &Error) -> ItemResult {
    ItemResult {
        request,
        name: None,
        r#ref: r,
        status: ItemStatus::Failed,
        skip_reason: None,
        duplicate_of: None,
        reason: None,
        from_version: None,
        to_version: None,
        folders: None,
        required_by: None,
        deps: ItemDeps::default(),
        error: Some(ItemError::from(error)),
    }
}

/// The file to read: `<file>` (relative to the working directory), else the default.
fn file_path(world: &World, args: &SyncArgs) -> PathBuf {
    match &args.file {
        Some(file) if file.is_relative() => std::env::current_dir()
            .map(|cwd| cwd.join(file))
            .unwrap_or_else(|_| file.clone()),
        Some(file) => file.clone(),
        None => world.paths.addons_toml(),
    }
}

/// The file's text; a missing file is `not_found`.
fn read(path: &Path, given: bool) -> Result<String> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(text),
        // ubs:ignore — an io::ErrorKind, not a secret
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            let fix = if given {
                Suggestion::text("pass the file: `omaddons sync <path>`")
            } else {
                Suggestion::text(format!(
                    "pass the file: `omaddons sync <path>`, or write {} (see `omaddons help sync`)",
                    path.display()
                ))
            };
            Err(Error::new(
                ErrorCode::NotFound,
                format!("{} does not exist", path.display()),
                fix,
            )
            // What the file holds, read-only.
            .alternative(Suggestion::command(["omaddons", "help", "sync"]))
            .details(json!({ "path": path.display().to_string() })))
        }
        Err(err) => Err(Error::io(
            path,
            &err,
            Suggestion::text(format!("check that {} can be read", path.display())),
        )),
    }
}

/// What the command decided for one target before the engine.
#[derive(Default)]
struct TargetWork {
    changes: Vec<RequestedChange>,
    requested: Vec<ItemRequest>,
    items: Vec<ItemResult>,
    /// Item requests in file order.
    order: Vec<String>,
    /// `--locked`: the dependency locks (`dependency = true` entries with a version), by
    /// resolved ref.
    locks: BTreeMap<Ref, String>,
    /// `--locked`: entries installed without a version (the newest release).
    unlocked: Vec<String>,
    /// `--locked`: the requests that install a recorded version.
    locked: Vec<String>,
}

/// Resolves, checks and diffs `target`'s entries; with `--prune`, removes the records
/// nothing keeps ([`keep_set`]).
fn target_work(
    state: &DesiredState,
    d: &DiscoveredTarget,
    records: &BTreeMap<String, TargetRecord>,
    catalog: &Catalog,
    world: &World,
    path: &Path,
    args: &SyncArgs,
) -> TargetWork {
    let locked = args.locked;
    // What the file names, or may name, on this target: never pruned.
    let mut named: Vec<Ref> = Vec::new();
    let t = &d.target;
    let base = path.parent().unwrap_or_else(|| Path::new("/"));
    let git_hosts = &world.config.git_hosts;
    let wanted = desired::desired_for(state, t, |e| {
        resolve_entry(e, (&t.id, t.family), records, catalog, git_hosts, base)
    });
    let at = |entry: &Entry| format!("{} line {}", path.display(), entry.line);
    let mut work = TargetWork::default();
    let mut wants: Vec<Want<'_>> = Vec::new();
    for d in wanted {
        // Locks are not requests: `--locked` fixes the version of a dependency it
        // installs; one that resolves to nothing is ignored.
        if d.entry.dependency {
            if let Ok(r) = &d.resolved {
                named.push(r.clone());
            }
            if let (true, Ok(r), Some(version)) = (locked, &d.resolved, &d.entry.version) {
                work.locks.insert(r.clone(), version.clone());
            }
            continue;
        }
        work.order.push(label(d.entry));
        match &d.resolved {
            Ok(r) => named.push(r.clone()),
            Err(error) => named.extend(ambiguous_refs(error)),
        }
        match d.resolved {
            Ok(r) => wants.push(Want {
                entry: d.entry,
                name: catalog_name(catalog, &r)
                    .or_else(|| d.entry.r#ref.as_ref().and_then(|_| d.entry.name.clone())),
                r#ref: r,
            }),
            Err(error) => {
                let mut error = error;
                error.message = format!("{}: {}", at(d.entry), error.message);
                work.items.push(failed_item(label(d.entry), None, &error));
            }
        }
    }
    let duplicates = resolved_duplicates(&wants);
    for (&i, &j) in &duplicates {
        let (a, b) = (&wants[i], &wants[j]);
        let (first, later) = (
            a.entry.line.min(b.entry.line),
            a.entry.line.max(b.entry.line),
        );
        let error = Error::new(
            ErrorCode::DesiredStateInvalid,
            format!(
                "{}: {} and {} (line {}) are both {}; neither is synced",
                at(a.entry),
                label(a.entry),
                label(b.entry),
                b.entry.line,
                a.r#ref
            ),
            Suggestion::text(format!(
                "keep one of the entries on lines {first} and {later} of {}",
                path.display()
            )),
        )
        .details(json!({
            "path": path.display().to_string(),
            "line": a.entry.line,
            "key": "",
            "reason": "duplicate",
            "other_line": b.entry.line,
        }));
        work.items
            .push(failed_item(label(a.entry), Some(a.r#ref.clone()), &error));
    }
    let wants: Vec<Want<'_>> = wants
        .into_iter()
        .enumerate()
        .filter(|(i, _)| !duplicates.contains_key(i))
        .map(|(_, w)| w)
        .collect();
    let record = records.get(&t.id);
    for (item, want) in diff(&wants, record, locked).into_iter().zip(&wants) {
        let request = label(item.entry);
        if let Some(RequestedChange::Install { version, .. }) = &item.change
            && locked
        {
            match version {
                Some(_) => work.locked.push(request.clone()),
                None if item.installed.is_none() => work.unlocked.push(request.clone()),
                None => {}
            }
        }
        match item.change {
            Some(change) => {
                work.changes.push(change);
                work.requested.push(ItemRequest {
                    request,
                    target: t.id.clone(),
                    addon: item
                        .installed
                        .map(|a| a.name.clone())
                        .or_else(|| want.name.clone()),
                    r#ref: Some(
                        item.installed
                            .map_or_else(|| want.r#ref.clone(), |a| a.r#ref.clone()),
                    ),
                    version: item.installed.map(|a| a.version.clone()),
                });
            }
            None => {
                let a = item.installed;
                work.items.push(ItemResult {
                    request,
                    name: a.map(|a| a.name.clone()),
                    r#ref: Some(a.map_or_else(|| want.r#ref.clone(), |a| a.r#ref.clone())),
                    status: ItemStatus::Unchanged,
                    skip_reason: None,
                    duplicate_of: None,
                    reason: None,
                    from_version: a.map(|a| a.version.clone()),
                    to_version: a.map(|a| a.version.clone()),
                    folders: None,
                    required_by: None,
                    deps: ItemDeps::default(),
                    error: None,
                });
            }
        }
    }
    // `--prune`: every record nothing keeps goes, by name, after the entries' changes.
    if let (true, Some(record)) = (args.prune, record) {
        let keep = keep_set(record, &named);
        let mut gone: Vec<&AddonRecord> = record
            .addons
            .iter()
            .filter(|(key, _)| !keep.contains(*key))
            .map(|(_, a)| a)
            .collect();
        gone.sort_by_key(|a| a.name.to_ascii_lowercase());
        for a in gone {
            work.changes.push(RequestedChange::Remove {
                addon: a.name.clone(),
            });
            work.requested.push(ItemRequest {
                request: a.name.clone(),
                target: t.id.clone(),
                addon: Some(a.name.clone()),
                r#ref: Some(a.r#ref.clone()),
                version: Some(a.version.clone()),
            });
            work.order.push(a.name.clone());
        }
    }
    work
}

/// Runs `sync`.
pub fn run(ctx: &Context, args: &SyncArgs) -> Result<SyncOutcome> {
    let world = World::load(ctx)?;
    let verbose = ctx.output.style.verbose > 0;
    let mut warnings = world.warnings.clone();
    let mut notes = Vec::new();
    let path = file_path(&world, args);
    let text = read(&path, args.file.is_some())?;
    // A file that cannot be read as desired state changes nothing.
    let (state, parsed) = desired::parse(&text, &path)?;
    warnings.extend(parsed);
    let known = world.resolved.known_ids();
    // Before anything is resolved: a section for no known client may be a typo whose
    // client --prune would empty.
    if args.prune
        && let Some(error) = unknown_section(&state, &known, &path)
    {
        return Err(error);
    }
    warnings.extend(desired::validate(&state, &known));
    let (catalog, catalog_warnings) = world.catalog(ctx);
    warnings.extend(catalog_warnings);
    let mut targets: Vec<&DiscoveredTarget> = world.select_many(ctx)?;
    targets.sort_by(|a, b| a.target.id.cmp(&b.target.id));
    let records = world.records()?;

    let mut changes: BTreeMap<String, Vec<RequestedChange>> = BTreeMap::new();
    let mut requested: Vec<ItemRequest> = Vec::new();
    let mut pushed: Vec<(TargetRef, ItemResult)> = Vec::new();
    let mut order: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut without: Vec<TargetResult> = Vec::new();
    let mut locks: BTreeMap<String, BTreeMap<Ref, String>> = BTreeMap::new();
    let mut locked: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut matching = 0usize;
    for d in &targets {
        if !has_section(&state, &d.target) {
            without.push(TargetResult {
                no_section: true,
                ..TargetResult::empty(target_ref(d))
            });
            continue;
        }
        matching += 1;
        let work = target_work(&state, d, &records, &catalog, &world, &path, args);
        if !work.unlocked.is_empty() {
            warnings.push(not_locked(&d.target.id, &work.unlocked, &path));
        }
        locks.insert(d.target.id.clone(), work.locks);
        locked.insert(d.target.id.clone(), work.locked);
        if !work.changes.is_empty() {
            changes.insert(d.target.id.clone(), work.changes);
        }
        requested.extend(work.requested);
        pushed.extend(work.items.into_iter().map(|i| (target_ref(d), i)));
        order.insert(d.target.id.clone(), work.order);
    }

    let run = RunId::new();
    // Nothing to change anywhere: no engine, no cache, no request (unless a crash left a
    // journal to recover).
    let done: Option<TxOutcomes> = if changes.is_empty() {
        recover_waiting(ctx, &world, "sync");
        None
    } else {
        let engine = Engine::new(ctx, &world, "sync")?;
        let freshness = if ctx.offline {
            Freshness::OfflineOnly
        } else {
            Freshness::Window(SOURCE_RECHECK_MIN_INTERVAL)
        };
        let cache = engine.cache(freshness);
        let req = TxRequest {
            targets: targets
                .iter()
                .filter_map(|d| {
                    Some(TxTarget {
                        discovered: (*d).clone(),
                        changes: changes.remove(&d.target.id)?,
                        default_kind: TxKind::Sync,
                        dependency_locks: locks.remove(&d.target.id).unwrap_or_default(),
                    })
                })
                .collect(),
            force: false,
            while_running: ctx.while_running,
            dry_run: ctx.dry_run,
            reason: ctx.reason.clone(),
            kind: Some(TxKind::Sync),
            run,
            no_deps: args.no_deps,
        };
        let done = engine.run(&req, Some(&cache));
        let remembered = cache.remember(done.as_ref().map_or(&[][..], |d| &d.plan.facts));
        warnings.extend(remembered.warnings);
        notes.extend(remembered.notes);
        Some(done?)
    };

    let mut data = match &done {
        Some(done) => {
            let mut data = TargetsData::build(done, run, &requested);
            settle_failures(&mut data, &ctx.argv);
            unlock_fixes(&mut data, &locked, &ctx.argv);
            for o in &done.outcomes {
                warnings.extend(o.warnings.iter().cloned());
                notes.extend(o.notes.iter().cloned());
            }
            warnings.extend(done.warnings.iter().cloned());
            data
        }
        None => TargetsData::assemble(&[], &[], &[], ctx.dry_run, run, &[]),
    };
    for (target, item) in pushed {
        data.push_item(target, item);
    }
    for target in without {
        data.push_target(target);
    }
    // Every selected target is listed; items in file order, each with its dependencies
    // right after it.
    for d in &targets {
        if !data.targets.iter().any(|t| t.target.id == d.target.id) {
            data.push_target(TargetResult::empty(target_ref(d)));
        }
    }
    for t in &mut data.targets {
        let Some(order) = order.get(&t.target.id) else {
            continue;
        };
        sort_keeping_dependencies(&mut t.items, |i| {
            order
                .iter()
                .position(|r| *r == i.request)
                .unwrap_or(order.len())
        });
    }
    for t in &data.targets {
        for i in t.items.iter().filter(|i| i.status == ItemStatus::Failed) {
            if let Some(e) = &i.error {
                let mut warning = Warning::item(&e.error());
                let name = i.name.as_deref().unwrap_or(&i.request);
                warning.message = format!("{}: {name}: {}", t.target.id, e.message);
                warnings.push(warning);
            }
        }
    }
    if let Some(error) = data.all_failed() {
        return Err(error);
    }
    let summary = data.summary;
    let mut footer = Vec::new();
    if summary.changed == 0 && summary.failed == 0 {
        let clients = if matching == 1 {
            "client matches"
        } else {
            "clients match"
        };
        footer.push(format!(
            "nothing to do ({matching} {clients} {})",
            path.display()
        ));
    }
    if summary.unchanged > 0 {
        footer.push("sync does not update; run `omaddons update`".to_owned());
    }
    let locks: Vec<(TxId, u64)> = done
        .iter()
        .flat_map(|d| &d.outcomes)
        .filter_map(|o| Some((o.tx?, o.lock_held_ms?)))
        .collect();
    let data = data.listing(verbose);
    let commands = data.undo_commands();
    let mut unique: Vec<Warning> = Vec::new();
    for w in warnings {
        if !unique.contains(&w) {
            unique.push(w);
        }
    }
    Ok(SyncOutcome {
        inner: TargetsOutcome {
            command: "sync",
            data,
            warnings: unique,
            commands,
            notes: footer,
        },
        strict: args.strict,
        locks,
        notes,
    })
}

/// `not_locked`: `entries` of `target` have no version, so `--locked` installs their
/// newest release.
fn not_locked(target: &str, entries: &[String], path: &Path) -> Warning {
    Warning::new(
        WarningCode::NotLocked,
        format!(
            "{target}: {} {} no version in {}, so the newest release installs",
            entries.join(", "),
            if entries.len() == 1 { "has" } else { "have" },
            path.display()
        ),
    )
    .with_fix(Suggestion::text(
        "add `version = \"…\"` to those entries, or regenerate the file with `omaddons export`",
    ))
}

/// A locked version the source no longer serves (`not_found`): the fix syncs to the
/// newest instead, which is this invocation without `--locked`.
fn unlock_fixes(data: &mut TargetsData, locked: &BTreeMap<String, Vec<String>>, argv: &[String]) {
    let fix: Vec<String> = crate::cli::fix_argv(argv)
        .into_iter()
        .filter(|a| !matches!(a.as_str(), "--locked"))
        .collect();
    let fix = Suggestion::command(fix);
    for t in &mut data.targets {
        let Some(requests) = locked.get(&t.target.id) else {
            continue;
        };
        for i in &mut t.items {
            if let Some(error) = &mut i.error
                && error.code == ErrorCode::NotFound
                && requests.contains(&i.request)
            {
                error.fix = fix.display().to_owned();
                error.fix_argv = fix.argv().map(<[String]>::to_vec);
            }
        }
    }
}

/// A fix that reruns this `sync` with `--force` (which `sync` does not have) says how
/// with `install`. (A dependency that failed is a failed item of its own already.)
fn settle_failures(data: &mut TargetsData, argv: &[String]) {
    // The engine's retry is this run's argv without its presentation flags.
    let argv = crate::cli::fix_argv(argv);
    for t in &mut data.targets {
        let id = t.target.id.clone();
        for i in &mut t.items {
            let Some(error) = &mut i.error else {
                continue;
            };
            let forced = error.fix_argv.as_ref().is_some_and(|fix| {
                fix.split_last()
                    .is_some_and(|(last, rest)| last == "--force" && rest == argv)
            });
            if !forced {
                continue;
            }
            let what = i.r#ref.as_ref().map_or_else(
                || i.name.clone().unwrap_or_else(|| i.request.clone()),
                ToString::to_string,
            );
            let fix = Suggestion::command([
                "omaddons".to_owned(),
                "install".to_owned(),
                what,
                "--force".to_owned(),
                "-t".to_owned(),
                id.clone(),
            ]);
            error.fix = fix.display().to_owned();
            error.fix_argv = fix.argv().map(<[String]>::to_vec);
        }
    }
    data.recount();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AddonState, Channel, TargetKey, VersionConfidence};

    fn record(addons: &[(&str, &str, &str, Channel, bool)]) -> TargetRecord {
        let mut record = TargetRecord::empty(TargetKey::new());
        for (name, r, version, channel, pinned) in addons {
            let a = AddonRecord {
                r#ref: r.parse().unwrap(),
                name: (*name).to_owned(),
                version: (*version).to_owned(),
                version_confidence: VersionConfidence::Release,
                release_id: None,
                channel: *channel,
                pinned: *pinned,
                reason: AddonReason::Explicit,
                required_by: Vec::new(),
                folders: Vec::new(),
                state: AddonState::Ok,
                installed_at: jiff::Timestamp::now(),
                tx_id: TxId::new(),
                blob_sha256: None,
                revision: None,
                left_out: Vec::new(),
                any_flavor: false,
            };
            record.addons.insert(TargetRecord::addon_key(name), a);
        }
        record
    }

    fn entry(name: &str, channel: Channel, version: Option<&str>, pinned: bool) -> Entry {
        Entry {
            channel,
            version: version.map(str::to_owned),
            pinned,
            ..Entry::named(name)
        }
    }

    fn want<'s>(e: &'s Entry, r: &str) -> Want<'s> {
        Want {
            entry: e,
            r#ref: r.parse().unwrap(),
            name: Some(e.request().to_owned()),
        }
    }

    /// What a change is: `(kind, version, pin or edit, channel)`.
    type Shape<'a> = (&'a str, Option<&'a str>, &'a str, &'a str);

    /// `(kind, version, pin, channel)` of an install, or `("edit", …)`, or `("none", …)`.
    fn shape(change: Option<&RequestedChange>) -> (&'static str, Option<String>, String, String) {
        match change {
            Some(RequestedChange::Install {
                version,
                pin,
                channel,
                channel_explicit,
                reason,
                ..
            }) => {
                assert!(*channel_explicit);
                assert_eq!(*reason, AddonReason::Explicit);
                (
                    "install",
                    version.clone(),
                    format!("{pin:?}"),
                    format!("{channel:?}"),
                )
            }
            Some(RequestedChange::Edit { edit, .. }) => {
                ("edit", None, format!("{edit:?}"), String::new())
            }
            Some(_) => ("other", None, String::new(), String::new()),
            None => ("none", None, String::new(), String::new()),
        }
    }

    #[test]
    fn prune_keeps_what_the_file_names_and_everything_that_requires() {
        use Channel::Stable;
        let mut rec = record(&[
            ("Little", "gh:o/little", "1", Stable, false),
            ("Big", "gh:o/big", "1", Stable, false),
            ("Lib", "gh:o/lib", "1", Stable, false),
            ("Orphan", "gh:o/orphan", "1", Stable, false),
            ("Other", "gh:o/other", "1", Stable, false),
            ("Locked", "gh:o/locked", "1", Stable, false),
            ("Dup", "gh:o/dup", "1", Stable, false),
            ("Amb", "gh:o/amb", "1", Stable, false),
        ]);
        let requires = |rec: &mut TargetRecord, addon: &str, by: &str| {
            rec.addons
                .get_mut(&TargetRecord::addon_key(addon))
                .unwrap()
                .required_by = vec![by.to_owned()];
        };
        // A chain Little → Big → Lib; a stale requester; one required only by a pruned one.
        requires(&mut rec, "Big", "little");
        requires(&mut rec, "Lib", "Big");
        requires(&mut rec, "Orphan", "Gone");
        requires(&mut rec, "Other", "Orphan");
        let refs: Vec<Ref> = ["gh:O/Little", "gh:o/locked", "gh:o/dup", "gh:o/amb"]
            .iter()
            .map(|r| r.parse().unwrap())
            .collect();
        let keep = keep_set(&rec, &refs);
        assert_eq!(
            keep.into_iter().collect::<Vec<_>>(),
            ["amb", "big", "dup", "lib", "little", "locked"]
        );
    }

    #[test]
    fn locked_installs_the_recorded_versions_without_pinning() {
        use Channel::{Beta, Stable};
        let installed = record(&[
            ("Foo", "gh:o/foo", "v1", Stable, false),
            ("Pinned", "gh:o/pinned", "v1", Stable, true),
        ]);
        let with_sha = |mut e: Entry| {
            e.sha256 = Some("ab".repeat(32));
            e
        };
        let cases: &[(Entry, &str, Shape<'_>, Shape<'_>)] = &[
            // Missing: its version whenever it has one, unpinned unless the entry pins.
            (
                entry("New", Stable, Some("v3"), false),
                "gh:o/new",
                ("install", Some("v3"), "Keep", "Stable"),
                ("install", None, "Keep", "Stable"),
            ),
            (
                entry("New", Stable, None, false),
                "gh:o/new",
                ("install", None, "Keep", "Stable"),
                ("install", None, "Keep", "Stable"),
            ),
            // Installed at another version: moved there, still unpinned.
            (
                entry("Foo", Stable, Some("v9"), false),
                "gh:o/foo",
                ("install", Some("v9"), "Keep", "Stable"),
                ("none", None, "", ""),
            ),
            // A pinned record whose entry is unpinned at another version: moved there and
            // unpinned (plain sync only unpins).
            (
                entry("Pinned", Stable, Some("v2"), false),
                "gh:o/pinned",
                ("install", Some("v2"), "Unpin", "Stable"),
                ("edit", None, "SetPinned(false)", ""),
            ),
            // Another channel: at its version.
            (
                entry("Foo", Beta, Some("v1"), false),
                "gh:o/foo",
                ("install", Some("v1"), "Keep", "Beta"),
                ("install", None, "Keep", "Beta"),
            ),
            // At its version already: nothing either way.
            (
                entry("Foo", Stable, Some("v1"), false),
                "gh:o/foo",
                ("none", None, "", ""),
                ("none", None, "", ""),
            ),
        ];
        for (e, r, locked, plain) in cases {
            for (on, expected) in [(true, locked), (false, plain)] {
                let items = diff(&[want(e, r)], Some(&installed), on);
                let got = shape(items[0].change.as_ref());
                assert_eq!(
                    (got.0, got.1.as_deref(), got.2.as_str(), got.3.as_str()),
                    *expected,
                    "locked {on}: {e:?} {r}"
                );
            }
        }
        // Only a locked install with a version carries the recorded archive.
        let sha = |e: Entry, on: bool| {
            let items = diff(&[want(&e, "gh:o/new")], None, on);
            match &items[0].change {
                Some(RequestedChange::Install { blob_sha256, .. }) => blob_sha256.clone(),
                _ => None,
            }
        };
        assert!(sha(with_sha(entry("New", Stable, Some("v3"), false)), true).is_some());
        assert!(sha(with_sha(entry("New", Stable, Some("v3"), false)), false).is_none());
        assert!(sha(with_sha(entry("New", Stable, None, false)), true).is_none());
    }

    #[test]
    fn the_diff_table() {
        use Channel::{Beta, Stable};
        let installed = record(&[
            ("Foo", "gh:o/foo", "v1", Stable, false),
            ("Pinned", "gh:o/pinned", "v1", Stable, true),
            ("Beta", "gh:o/beta", "v1", Beta, false),
        ]);
        let cases: &[(Entry, &str, Shape<'_>)] = &[
            // Missing: installed, its version only when pinned.
            (
                entry("New", Stable, Some("v3"), false),
                "gh:o/new",
                ("install", None, "Keep", "Stable"),
            ),
            (
                entry("New", Beta, Some("v3"), true),
                "gh:o/new",
                ("install", Some("v3"), "Pin", "Beta"),
            ),
            // Installed, current entry: nothing, whatever its unpinned version says.
            (
                entry("Foo", Stable, None, false),
                "gh:o/foo",
                ("none", None, "", ""),
            ),
            (
                entry("Foo", Stable, Some("v9"), false),
                "gh:o/foo",
                ("none", None, "", ""),
            ),
            // Pinned at another version: that version, pinned.
            (
                entry("Foo", Stable, Some("v2"), true),
                "gh:o/foo",
                ("install", Some("v2"), "Pin", "Stable"),
            ),
            // Pinned at its version, or pinned without one: a records-only pin.
            (
                entry("Foo", Stable, Some("v1"), true),
                "gh:o/foo",
                ("edit", None, "SetPinned(true)", ""),
            ),
            (
                entry("Foo", Stable, None, true),
                "gh:o/foo",
                ("edit", None, "SetPinned(true)", ""),
            ),
            // Unpinned entry against a pinned addon: unpin, no install.
            (
                entry("Pinned", Stable, None, false),
                "gh:o/pinned",
                ("edit", None, "SetPinned(false)", ""),
            ),
            // Another channel: switched, the pin carried as an intent.
            (
                entry("Foo", Beta, None, false),
                "gh:o/foo",
                ("install", None, "Keep", "Beta"),
            ),
            (
                entry("Beta", Stable, None, true),
                "gh:o/beta",
                ("install", None, "Pin", "Stable"),
            ),
            (
                entry("Pinned", Beta, None, false),
                "gh:o/pinned",
                ("install", None, "Unpin", "Beta"),
            ),
            // A pinned version on another channel: the version row wins.
            (
                entry("Beta", Stable, Some("v2"), true),
                "gh:o/beta",
                ("install", Some("v2"), "Pin", "Stable"),
            ),
            // A ref in another case is the same addon.
            (
                entry("Foo", Stable, None, false),
                "gh:O/Foo",
                ("none", None, "", ""),
            ),
        ];
        for (e, r, (kind, version, pin, channel)) in cases {
            let items = diff(&[want(e, r)], Some(&installed), false);
            let got = shape(items[0].change.as_ref());
            assert_eq!(
                (got.0, got.1.as_deref(), got.2.as_str(), got.3.as_str()),
                (*kind, *version, *pin, *channel),
                "{e:?} {r}"
            );
        }
    }

    #[test]
    fn an_existing_addon_is_named_and_reffed_as_recorded() {
        let installed = record(&[("Foo", "gh:o/foo", "v1", Channel::Stable, false)]);
        let e = entry("whatever", Channel::Beta, None, false);
        let items = diff(&[want(&e, "gh:O/FOO")], Some(&installed), false);
        assert!(
            matches!(
                items[0].change.as_ref(),
                Some(RequestedChange::Install { r#ref, name, .. })
                    if r#ref.to_string() == "gh:o/foo" && name.as_deref() == Some("Foo")
            ),
            "{:?}",
            items[0].change
        );
        // Nothing installed, no record at all: installs, under the given name.
        let items = diff(&[want(&e, "gh:o/new")], None, false);
        assert!(items[0].installed.is_none());
        assert!(matches!(
            items[0].change,
            Some(RequestedChange::Install { ref name, .. }) if name.as_deref() == Some("whatever")
        ));
    }

    #[test]
    fn entries_resolving_to_one_ref_are_all_duplicates() {
        let (a, b, c) = (
            Entry::named("BigWigs"),
            Entry::named("gh:BigWigsMods/BigWigs"),
            Entry::named("DBM"),
        );
        let wants = [
            want(&a, "gh:BigWigsMods/BigWigs"),
            want(&b, "gh:bigwigsmods/bigwigs"),
            want(&c, "gh:o/dbm"),
        ];
        let dups = resolved_duplicates(&wants);
        assert_eq!(dups, BTreeMap::from([(0, 1), (1, 0)]));
    }
}
