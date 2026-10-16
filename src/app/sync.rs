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

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::json;

use super::engine::Engine;
use super::outcome::{
    ItemError, ItemRequest, ItemResult, ItemStatus, TargetRef, TargetResult, TargetsData,
    TargetsOutcome,
};
use super::resolve_args::{catalog_name, resolve_entry};
use super::world::World;
use crate::cli::Context;
use crate::discover::DiscoveredTarget;
use crate::engine::edit::RecordEdit;
use crate::engine::plan::{PinIntent, RequestedChange};
use crate::engine::{TxOutcomes, TxRequest, TxTarget};
use crate::error::{CommandHint, Error, ErrorCode, Result, Suggestion, Warning};
use crate::model::{AddonReason, AddonRecord, Ref, RunId, Target, TxId, TxKind};
use crate::output::{Outcome, Style};
use crate::politeness::SOURCE_RECHECK_MIN_INTERVAL;
use crate::resolve::cache::Freshness;
use crate::resolve::catalog::Catalog;
use crate::rules::TargetKind;
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
/// | situation | change |
/// |---|---|
/// | not installed | install (its `version` only when pinned) |
/// | pinned at another `version` | install that version, pinned |
/// | another `channel` | install from that channel (a records-only switch when its newest release is installed) |
/// | only the pin differs | records-only `SetPinned` |
/// | otherwise | nothing |
pub fn diff<'s, 'r>(wants: &[Want<'s>], record: Option<&'r TargetRecord>) -> Vec<DiffItem<'s, 'r>> {
    wants
        .iter()
        .map(|w| {
            let e = w.entry;
            let installed = record.and_then(|r| {
                r.addons
                    .values()
                    .find(|a| a.r#ref.identity() == w.r#ref.identity())
            });
            let install = |r#ref: Ref, name: Option<String>, version, pin| {
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
                })
            };
            let pinned_version = || e.version.clone().filter(|_| e.pinned);
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
                    if e.pinned && e.version.as_ref().is_some_and(|v| *v != a.version) {
                        let (r, name) = again();
                        install(r, name, e.version.clone(), PinIntent::Pin)
                    } else if e.channel != a.channel {
                        let (r, name) = again();
                        install(r, name, pinned_version(), pin)
                    } else if e.pinned != a.pinned {
                        Some(RequestedChange::Edit {
                            addon: a.name.clone(),
                            edit: RecordEdit::SetPinned(e.pinned),
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
}

/// Resolves, checks and diffs `target`'s entries.
fn target_work(
    state: &DesiredState,
    d: &DiscoveredTarget,
    records: &BTreeMap<String, TargetRecord>,
    catalog: &Catalog,
    world: &World,
    path: &Path,
) -> TargetWork {
    let t = &d.target;
    let base = path.parent().unwrap_or_else(|| Path::new("/"));
    let git_hosts = &world.config.git_hosts;
    let wanted = desired::desired_for(state, t, |e| {
        resolve_entry(e, &t.id, records, catalog, git_hosts, base)
    });
    let at = |entry: &Entry| format!("{} line {}", path.display(), entry.line);
    let mut work = TargetWork::default();
    let mut wants: Vec<Want<'_>> = Vec::new();
    for d in wanted {
        // Locks are not requests (`--locked` reads them).
        if d.entry.dependency {
            continue;
        }
        work.order.push(label(d.entry));
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
    for (item, want) in diff(&wants, record).into_iter().zip(&wants) {
        let request = label(item.entry);
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
                    error: None,
                });
            }
        }
    }
    work
}

/// Runs `sync`.
pub fn run(ctx: &Context, args: &SyncArgs) -> Result<SyncOutcome> {
    let world = World::load()?;
    let verbose = ctx.output.style.verbose > 0;
    let mut warnings = world.warnings.clone();
    let mut notes = Vec::new();
    let path = file_path(&world, args);
    let text = read(&path, args.file.is_some())?;
    // A file that cannot be read as desired state changes nothing.
    let (state, parsed) = desired::parse(&text, &path)?;
    warnings.extend(parsed);
    warnings.extend(desired::validate(&state, &world.resolved.known_ids()));
    let (catalog, catalog_warnings) = Catalog::load(&world.paths);
    warnings.extend(catalog_warnings);
    let mut targets: Vec<&DiscoveredTarget> = world.select_many(ctx)?;
    targets.sort_by(|a, b| a.target.id.cmp(&b.target.id));
    let records = world.records()?;

    let mut changes: BTreeMap<String, Vec<RequestedChange>> = BTreeMap::new();
    let mut requested: Vec<ItemRequest> = Vec::new();
    let mut pushed: Vec<(TargetRef, ItemResult)> = Vec::new();
    let mut order: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut without: Vec<TargetResult> = Vec::new();
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
        let work = target_work(&state, d, &records, &catalog, &world, &path);
        if !work.changes.is_empty() {
            changes.insert(d.target.id.clone(), work.changes);
        }
        requested.extend(work.requested);
        pushed.extend(work.items.into_iter().map(|i| (target_ref(d), i)));
        order.insert(d.target.id.clone(), work.order);
    }

    let run = RunId::new();
    // Nothing to change anywhere: no engine, no cache, no request.
    let done: Option<TxOutcomes> = if changes.is_empty() {
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
                        dependency_locks: BTreeMap::new(),
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
            settle_failures(&mut data, done, &ctx.argv);
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
    // Every selected target is listed; items in file order, dependencies last.
    for d in &targets {
        if !data.targets.iter().any(|t| t.target.id == d.target.id) {
            data.push_target(TargetResult::empty(target_ref(d)));
        }
    }
    for t in &mut data.targets {
        let Some(order) = order.get(&t.target.id) else {
            continue;
        };
        t.items.sort_by_key(|i| {
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
        let clients = if matching == 1 { "client" } else { "clients" };
        footer.push(format!(
            "nothing to do ({matching} {clients} match {})",
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

/// What the engine's failures mean for `sync`: a dependency that resolved but failed
/// is a failed item of its own (M2-D9); a fix that reruns this `sync` with `--force`
/// (which `sync` does not have) says how with `install`.
fn settle_failures(data: &mut TargetsData, done: &TxOutcomes, argv: &[String]) {
    for f in done
        .failures
        .iter()
        .filter(|f| f.error.details.get("required_by").is_some())
    {
        let Some(t) = data.targets.iter().find(|t| t.target.id == f.target) else {
            continue;
        };
        let target = t.target.clone();
        let request = f
            .r#ref
            .as_ref()
            .map_or_else(|| "a dependency".to_owned(), ToString::to_string);
        let mut item = ItemResult {
            error: Some(ItemError::from(&f.error)),
            ..failed_item(request, f.r#ref.clone(), &Error::internal(""))
        };
        item.name = f.addon.clone();
        data.push_item(target, item);
    }
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
            let items = diff(&[want(e, r)], Some(&installed));
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
        let items = diff(&[want(&e, "gh:O/FOO")], Some(&installed));
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
        let items = diff(&[want(&e, "gh:o/new")], None);
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
