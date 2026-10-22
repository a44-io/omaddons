//! `omaddons update [<addon>…] [--all] [-t T] [--channel C] [--refresh] [--strict]`
//! (alias `upgrade`): every managed addon on every client, or the named ones, to its
//! newest compatible release (M2-D4: bare `update` is `--all`).
//!
//! What to update comes from the shared update-status evaluator, exactly as `outdated`
//! sees it: one fetch per distinct ref and channel, from the release memo when it is
//! fresh. Each target with updates commits its own transaction, all under one run, so
//! `omaddons undo` reverts the whole run. Pinned, ahead, blocked, static and unchecked
//! addons are skipped and said so; a newer release that turns out not to load on a
//! client is skipped as blocked, not failed (M2-D41). A fetch that fails is a failed
//! item: a mutating command never installs from stale metadata.

use std::collections::BTreeMap;

use super::adopt::hints::{adopt_fixes, adopting};
use super::engine::{Engine, recover_waiting};
use super::outcome::{
    ItemDeps, ItemError, ItemRequest, ItemResult, ItemStatus, SkipReason, TargetRef, TargetResult,
    TargetsData, TargetsOutcome, sort_keeping_dependencies,
};
use super::world::{World, names_addon};
use crate::cli::Context;
use crate::discover::DiscoveredTarget;
use crate::engine::plan::{PinIntent, RequestedChange, addon_pinned};
use crate::engine::{TxOutcomes, TxRequest, TxTarget};
use crate::error::{CommandHint, Error, ErrorBody, ErrorCode, Result, Suggestion, Warning};
use crate::model::{AddonRecord, Channel, Ref, RunId, TxId, TxKind};
use crate::output::{Outcome, Style};
use crate::politeness::SOURCE_RECHECK_MIN_INTERVAL;
use crate::resolve::cache::Freshness;
use crate::resolve::catalog::Catalog;
use crate::resolve::updates::{
    UncheckedReason, UpdateStatus, installed_primary_interface, is_static, statuses, update_floor,
};
use crate::sources::{SourceId, Wanted};
use crate::state::records::TargetRecord;

/// `update`'s own arguments.
#[derive(Debug, Clone, Default, clap::Args)]
pub struct UpdateArgs {
    /// Only these installed addons (names, or catalog names and aliases); default: all
    #[arg(value_name = "ADDON")]
    pub addons: Vec<String>,
    /// Every managed addon on every selected client (the default)
    #[arg(long)]
    pub all: bool,
    /// Switch the named addons to stable, beta or alpha and install that channel's newest
    /// release
    #[arg(long, value_name = "CHANNEL", value_parser = ["stable", "beta", "alpha"])]
    pub channel: Option<String>,
    /// Ask every source now, even when it was checked in the last 15 minutes
    #[arg(long)]
    pub refresh: bool,
    /// Exit 6 when only some of the addons update
    #[arg(long)]
    pub strict: bool,
    /// Update only the named addons, not the ones their TOCs require
    #[arg(long)]
    pub no_deps: bool,
}

/// `update`'s outcome: the shared multi-target outcome, with `--strict`.
#[derive(Debug)]
pub struct UpdateOutcome {
    inner: TargetsOutcome,
    strict: bool,
    /// Each committed transaction's global lock hold time, for `-v` log lines.
    pub locks: Vec<(TxId, u64)>,
    /// For `-v` log lines: why addons were left alone, the memo ignored or not written.
    pub notes: Vec<String>,
}

impl Outcome for UpdateOutcome {
    type Data = TargetsData;

    fn command(&self) -> &'static str {
        "update"
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
            Error::new(
                ErrorCode::CheckFailed,
                format!("{failed} {} did not update (--strict)", noun(failed)),
                Suggestion::text("see data.targets[].items[].error for each failed addon"),
            )
        })
    }

    fn render_human(&self, w: &mut dyn std::io::Write, style: &Style) -> std::io::Result<()> {
        self.inner.render_human(w, style)
    }
}

fn noun(n: usize) -> &'static str {
    if n == 1 { "addon" } else { "addons" }
}

/// One managed addon in the run's scope.
struct InScope<'w> {
    target: &'w DiscoveredTarget,
    record: &'w AddonRecord,
    /// What the user typed for it (its name for bare `update`).
    request: String,
    /// The user named it.
    named: bool,
}

/// Runs `update`.
pub fn run(ctx: &Context, args: &UpdateArgs) -> Result<UpdateOutcome> {
    let channel = check_args(ctx, args)?;
    let world = World::load(ctx)?;
    let verbose = ctx.output.style.verbose > 0;
    let mut warnings = world.warnings.clone();
    let mut notes = Vec::new();
    let (catalog, catalog_warnings) = world.catalog(ctx);
    warnings.extend(catalog_warnings);
    let mut targets: Vec<&DiscoveredTarget> = world.select_many(ctx)?;
    targets.sort_by(|a, b| a.target.id.cmp(&b.target.id));
    let records = world.records()?;
    let (scope, unknown) = in_scope(&targets, &records, &catalog, &args.addons);

    let engine = Engine::new(ctx, &world, "update")?;
    let enabled = engine.enabled_sources();
    // `--offline` wins over `--refresh`.
    let freshness = if ctx.offline {
        Freshness::OfflineOnly
    } else if args.refresh {
        Freshness::Refresh
    } else {
        Freshness::Window(SOURCE_RECHECK_MIN_INTERVAL)
    };
    let cache = engine.cache(freshness);

    // A named addon switched to another channel skips the evaluator: the user asked for
    // that channel's newest release, older or not.
    let switches = |s: &InScope<'_>| channel.is_some_and(|c| s.named && c != s.record.channel);
    let mut wanted: Vec<Wanted> = Vec::new();
    for s in scope.iter().filter(|s| !switches(s)) {
        let a = s.record;
        let w = Wanted {
            r#ref: a.r#ref.clone(),
            channel: a.channel,
            version: None,
        };
        let skip = is_static(a)
            || !enabled.contains(&SourceId::of(&a.r#ref))
            || (ctx.offline && cache.entry(&a.r#ref, a.channel).is_none());
        if !skip && !wanted.contains(&w) {
            wanted.push(w);
        }
    }
    // The cache reports each ref it asks of a source as it answers.
    let answers = cache.get(&wanted);
    // No stale fallback: a fetch that failed (and was not a remembered failure) fails
    // its items.
    let mut unanswered: BTreeMap<(Ref, Channel), Error> = BTreeMap::new();
    for (w, f) in &answers {
        if let Err(error) = &f.set {
            let remembered = cache
                .entry(&w.r#ref, w.channel)
                .is_some_and(|e| e.failure.is_some() && e.fetched_at >= f.fetched_at);
            if !remembered {
                unanswered.insert((w.r#ref.clone(), w.channel), error.clone());
            }
        }
    }

    // Every in-scope addon's status, from the shared evaluator.
    let pairs: Vec<(&crate::model::Target, TargetRecord)> = targets
        .iter()
        .filter_map(|d| {
            let record = records.get(&d.target.id)?;
            let mut only = record.clone();
            only.addons.retain(|_, a| {
                scope.iter().any(|s| {
                    s.target.target.id == d.target.id && s.record.name == a.name && !switches(s)
                })
            });
            Some((&d.target, only))
        })
        .collect();
    let pair_refs: Vec<(&crate::model::Target, &TargetRecord)> =
        pairs.iter().map(|(t, r)| (*t, r)).collect();
    let rows = statuses(
        &pair_refs,
        &|r, c| cache.entry(r, c),
        cache.facts(),
        &enabled,
        &|target, record| installed_primary_interface(target, record, &world.table),
    );

    // Plan the updates; say what is left alone.
    let mut changes: BTreeMap<String, Vec<RequestedChange>> = BTreeMap::new();
    let mut requested: Vec<ItemRequest> = Vec::new();
    let mut pushed: Vec<(TargetRef, ItemResult)> = Vec::new();
    for s in &scope {
        let a = s.record;
        let id = &s.target.target.id;
        let target_ref = TargetRef {
            id: id.clone(),
            key: s.target.target.key,
            mode: None,
        };
        let change = if switches(s) {
            Some(RequestedChange::Install {
                r#ref: a.r#ref.clone(),
                channel: channel.unwrap_or(a.channel),
                channel_explicit: true,
                version: None,
                file: None,
                name: Some(a.name.clone()),
                pin: PinIntent::Keep,
                reason: a.reason,
                reinstall: false,
                blob_sha256: None,
                asset: None,
                revision: None,
            })
        } else if let Some(error) = unanswered.get(&(a.r#ref.clone(), a.channel)) {
            pushed.push((target_ref, failed(s, ItemError::from(error))));
            None
        } else {
            let status = rows
                .iter()
                .find(|r| r.target == *id && r.addon.eq_ignore_ascii_case(&a.name))
                .map(|r| r.status.clone());
            match status {
                Some(UpdateStatus::UpdateAvailable { .. }) => Some(RequestedChange::Update {
                    addon: a.name.clone(),
                    installed_primary_interface: update_floor(&s.target.target, a, &world.table),
                }),
                Some(UpdateStatus::Pinned { .. }) => {
                    if s.named {
                        warnings.push(addon_pinned(a, id));
                    }
                    pushed.push((target_ref, skipped(s, SkipReason::Pinned)));
                    None
                }
                Some(UpdateStatus::Ahead { .. }) => {
                    pushed.push((target_ref, skipped(s, SkipReason::Ahead)));
                    None
                }
                Some(UpdateStatus::Blocked { .. }) => {
                    pushed.push((target_ref, skipped(s, SkipReason::Blocked)));
                    None
                }
                Some(UpdateStatus::Static) => {
                    pushed.push((target_ref, skipped(s, SkipReason::Static)));
                    None
                }
                Some(UpdateStatus::Unchecked { reason }) => {
                    if reason == UncheckedReason::NeverChecked && ctx.offline {
                        notes.push(format!("{id}: {} was never checked (offline)", a.name));
                    }
                    pushed.push((target_ref, skipped(s, SkipReason::Unchecked)));
                    None
                }
                Some(UpdateStatus::Failed { error }) => {
                    pushed.push((target_ref, failed(s, ItemError::from(&error))));
                    None
                }
                Some(UpdateStatus::Current { .. }) | None => {
                    pushed.push((target_ref, unchanged(s)));
                    None
                }
            }
        };
        if let Some(change) = change {
            changes.entry(id.clone()).or_default().push(change);
            requested.push(ItemRequest {
                request: s.request.clone(),
                target: id.clone(),
                addon: Some(a.name.clone()),
                r#ref: Some(a.r#ref.clone()),
                version: Some(a.version.clone()),
            });
        }
    }

    let run = RunId::new();
    let done: Option<TxOutcomes> = if changes.is_empty() {
        recover_waiting(ctx, &world, "update");
        None
    } else {
        let req = TxRequest {
            targets: targets
                .iter()
                .filter_map(|d| {
                    Some(TxTarget {
                        discovered: (*d).clone(),
                        changes: changes.remove(&d.target.id)?,
                        default_kind: TxKind::Update,
                        dependency_locks: BTreeMap::new(),
                    })
                })
                .collect(),
            force: false,
            while_running: ctx.while_running,
            dry_run: ctx.dry_run,
            reason: ctx.reason.clone(),
            kind: None,
            run,
            no_deps: args.no_deps,
        };
        match engine.run(&req, Some(&cache)) {
            Ok(done) => Some(done),
            Err(error) => {
                // What the plan fetched is still worth remembering.
                cache.remember(&[]);
                return Err(error);
            }
        }
    };
    let remembered = cache.remember(done.as_ref().map_or(&[][..], |d| &d.plan.facts));
    warnings.extend(remembered.warnings);
    notes.extend(remembered.notes);

    let mut data = match &done {
        Some(done) => {
            let mut data = TargetsData::build(done, run, &requested);
            settle_failures(&mut data, done);
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
    if let Some(first) = targets.first() {
        for name in &unknown {
            let checked: Vec<&str> = targets.iter().map(|d| d.target.id.as_str()).collect();
            let error = adopting(
                Error::new(
                    ErrorCode::NotManaged,
                    format!("{name} is not installed on {}", checked.join(", ")),
                    Suggestion::command(["omaddons", "install", name.as_str()]),
                ),
                adopt_fixes(&world, &records, &targets, name),
            );
            data.push_item(
                TargetRef {
                    id: first.target.id.clone(),
                    key: first.target.key,
                    mode: None,
                },
                ItemResult {
                    request: name.clone(),
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
                    error: Some(ItemError::from(&error)),
                },
            );
        }
    }
    // Every selected target is listed, with or without work; items by name.
    for d in &targets {
        if !data.targets.iter().any(|t| t.target.id == d.target.id) {
            data.push_target(TargetResult::empty(TargetRef {
                id: d.target.id.clone(),
                key: d.target.key,
                mode: None,
            }));
        }
    }
    // By name, each with its dependencies right after it.
    for t in &mut data.targets {
        sort_keeping_dependencies(&mut t.items, |i| {
            i.name.as_deref().unwrap_or(&i.request).to_lowercase()
        });
    }
    // Each failed item, mirrored as a warning (M1 install's rule).
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
    let locks: Vec<(TxId, u64)> = done
        .iter()
        .flat_map(|d| &d.outcomes)
        .filter_map(|o| Some((o.tx?, o.lock_held_ms?)))
        .collect();
    let data = data.listing(verbose);
    let mut commands = data.undo_commands();
    commands.extend(super::status::adopt_hints(&world, &targets, &records));
    let mut unique: Vec<Warning> = Vec::new();
    for w in warnings {
        if !unique.contains(&w) {
            unique.push(w);
        }
    }
    Ok(UpdateOutcome {
        inner: TargetsOutcome {
            command: "update",
            data,
            warnings: unique,
            commands,
            notes: Vec::new(),
        },
        strict: args.strict,
        locks,
        notes,
    })
}

/// `--all` with names, and `--channel` without names, are usage errors; the channel.
fn check_args(ctx: &Context, args: &UpdateArgs) -> Result<Option<Channel>> {
    if args.all && !args.addons.is_empty() {
        let argv = crate::cli::without_switch(&crate::cli::fix_argv(&ctx.argv), "--all");
        return Err(Error::new(
            ErrorCode::Usage,
            "--all updates every addon: name addons or give --all, not both",
            Suggestion::command(argv),
        ));
    }
    let channel = args.channel.as_deref().map(|c| match c {
        "beta" => Channel::Beta,
        "alpha" => Channel::Alpha,
        _ => Channel::Stable,
    });
    if let (Some(c), true) = (&args.channel, args.addons.is_empty()) {
        return Err(Error::new(
            ErrorCode::Usage,
            "--channel switches the addons you name to another channel: name them",
            Suggestion::text(format!("omaddons update <addon> --channel {c}")),
        ));
    }
    Ok(channel)
}

/// The managed addons in scope, by target id then name, and the names installed on no
/// selected target. A name matches a record's name (any case), or a catalog name or
/// alias whose ref is the record's.
fn in_scope<'w>(
    targets: &[&'w DiscoveredTarget],
    records: &'w BTreeMap<String, TargetRecord>,
    catalog: &Catalog,
    names: &[String],
) -> (Vec<InScope<'w>>, Vec<String>) {
    let mut scope = Vec::new();
    let mut unknown = Vec::new();
    let matches = |a: &AddonRecord, name: &str| names_addon(name, a, catalog);
    if names.is_empty() {
        for d in targets {
            let Some(record) = records.get(&d.target.id) else {
                continue;
            };
            let mut addons: Vec<&AddonRecord> = record.addons.values().collect();
            addons.sort_by_key(|a| a.name.to_lowercase());
            for a in addons {
                scope.push(InScope {
                    target: d,
                    record: a,
                    request: a.name.clone(),
                    named: false,
                });
            }
        }
        return (scope, unknown);
    }
    for name in names {
        let mut found = false;
        for d in targets {
            let Some(record) = records.get(&d.target.id) else {
                continue;
            };
            for a in record.addons.values().filter(|a| matches(a, name)) {
                found = true;
                if !scope.iter().any(|s: &InScope<'_>| {
                    s.target.target.id == d.target.id && s.record.name == a.name
                }) {
                    scope.push(InScope {
                        target: d,
                        record: a,
                        request: name.clone(),
                        named: true,
                    });
                }
            }
        }
        if !found {
            unknown.push(name.clone());
        }
    }
    (scope, unknown)
}

fn item(s: &InScope<'_>, status: ItemStatus) -> ItemResult {
    ItemResult {
        request: s.request.clone(),
        name: Some(s.record.name.clone()),
        r#ref: Some(s.record.r#ref.clone()),
        status,
        skip_reason: None,
        duplicate_of: None,
        reason: None,
        from_version: Some(s.record.version.clone()),
        to_version: None,
        folders: None,
        required_by: None,
        deps: ItemDeps::default(),
        error: None,
    }
}

fn skipped(s: &InScope<'_>, reason: SkipReason) -> ItemResult {
    ItemResult {
        skip_reason: Some(reason),
        ..item(s, ItemStatus::Skipped)
    }
}

fn unchanged(s: &InScope<'_>) -> ItemResult {
    ItemResult {
        to_version: Some(s.record.version.clone()),
        ..item(s, ItemStatus::Unchanged)
    }
}

fn failed(s: &InScope<'_>, error: ItemError) -> ItemResult {
    ItemResult {
        error: Some(error),
        ..item(s, ItemStatus::Failed)
    }
}

/// What the engine's failures mean for `update`: a release of an addon it was asked
/// to update that does not load here is skipped as blocked (M2-D41), never a
/// dependency's, which is a failure (M2-D9); a fix that takes over a folder with
/// `--force` says how with `install`, since `update` has no `--force`.
fn settle_failures(data: &mut TargetsData, done: &TxOutcomes) {
    for t in &mut data.targets {
        let id = t.target.id.clone();
        for i in &mut t.items {
            if i.status != ItemStatus::Failed {
                continue;
            }
            let failure = done.failures.iter().find(|f| {
                f.target == id
                    && (f.r#ref.is_some() && f.r#ref == i.r#ref
                        || f.addon.as_deref().is_some_and(|a| {
                            i.name.as_deref().is_some_and(|n| n.eq_ignore_ascii_case(a))
                        }))
            });
            let dependency = |f: &&crate::engine::plan::PlanFailure| {
                f.error.details.get("required_by").is_some()
            };
            if failure.is_some_and(|f| blocked(&f.error) && !dependency(&f)) {
                i.status = ItemStatus::Skipped;
                i.skip_reason = Some(SkipReason::Blocked);
                i.error = None;
                continue;
            }
            if let Some(error) = &mut i.error
                && error
                    .fix_argv
                    .as_ref()
                    .is_some_and(|argv| argv.iter().any(|a| a == "--force"))
            {
                let name = i.name.clone().unwrap_or_else(|| i.request.clone());
                let fix = Suggestion::command([
                    "omaddons".to_owned(),
                    "install".to_owned(),
                    name,
                    "--force".to_owned(),
                    "-t".to_owned(),
                    id.clone(),
                ]);
                error.fix = fix.display().to_owned();
                error.fix_argv = fix.argv().map(<[String]>::to_vec);
            }
        }
    }
    data.recount();
}

/// A verification refusal: the release exists but no build of it loads here (or none
/// is as new as an adopted build). A download or read failure is not one.
fn blocked(error: &ErrorBody) -> bool {
    let refusal = |reason: &str| {
        reason.starts_with("wrong_flavor") || reason.starts_with("older_than_installed")
    };
    match error.code {
        ErrorCode::WrongFlavor => true,
        ErrorCode::NoCompatibleRelease => error.details["tried"].as_array().is_some_and(|tried| {
            !tried.is_empty()
                && tried
                    .iter()
                    .all(|t| t["reason"].as_str().is_some_and(refusal))
        }),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_verification_refusals_are_blocked() {
        let body = |code: ErrorCode, tried: serde_json::Value| {
            ErrorBody::from(
                Error::new(code, "x", Suggestion::text("y"))
                    .details(serde_json::json!({ "tried": tried })),
            )
        };
        let reasons = |r: &[&str]| -> serde_json::Value {
            r.iter()
                .map(|r| serde_json::json!({"asset": "a.zip", "reason": r}))
                .collect()
        };
        assert!(blocked(&body(
            ErrorCode::WrongFlavor,
            serde_json::json!(null)
        )));
        assert!(blocked(&body(
            ErrorCode::NoCompatibleRelease,
            reasons(&["wrong_flavor: Foo: …", "older_than_installed: …"])
        )));
        assert!(!blocked(&body(
            ErrorCode::NoCompatibleRelease,
            reasons(&["wrong_flavor: …", "network: connection reset"])
        )));
        assert!(!blocked(&body(
            ErrorCode::NoCompatibleRelease,
            reasons(&[])
        )));
        assert!(!blocked(&body(
            ErrorCode::Network,
            reasons(&["wrong_flavor"])
        )));
    }
}
