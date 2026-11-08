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
use super::approval::{Approval, ApprovalFlags};
use super::engine::{Engine, recover_waiting};
use super::intent::{self, Preview, Storable, StorableArgs, TargetScope};
use super::outcome::{
    ItemDeps, ItemError, ItemRequest, ItemResult, ItemStatus, SkipReason, TargetRef, TargetResult,
    TargetsData, TargetsOutcome, sort_keeping_dependencies,
};
use super::precondition::{self, Idle, PlanBinding, PlanExtras, Precondition};
use super::world::{World, names_addon};
use crate::cli::Context;
use crate::discover::DiscoveredTarget;
use crate::engine::plan::{AnyFlavorIntent, PinIntent, RequestedChange, addon_pinned};
use crate::engine::{Binding, TxOutcomes, TxRequest, TxTarget};
use crate::error::{CommandHint, Error, ErrorBody, ErrorCode, Result, Suggestion, Warning};
use crate::model::{AddonRecord, Channel, Ref, RunId, Target, TxId, TxKind};
use crate::output::{Outcome, Style};
use crate::politeness::SOURCE_RECHECK_MIN_INTERVAL;
use crate::resolve::cache::Freshness;
use crate::resolve::catalog::Catalog;
use crate::resolve::updates::{
    UncheckedReason, UpdateStatus, installed_primary_interface, is_static, statuses, update_floor,
};
use crate::rules::TargetKind;
use crate::sources::{SourceId, Wanted};
use crate::state::records::TargetRecord;

/// `update`'s own arguments.
#[derive(Debug, Clone, Default, clap::Args)]
pub struct UpdateArgs {
    /// Only these installed addons; default: every one
    ///
    /// By name, or by catalog name or alias.
    #[arg(value_name = "ADDON")]
    pub addons: Vec<String>,
    /// Every managed addon on every selected client (the default)
    #[arg(long)]
    pub all: bool,
    /// Switch the named addons to this channel and install its newest
    ///
    /// The newest release of that channel installs, and the channel is recorded.
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
    /// `-t all`'s first human line ([`all_line`]); not serialized.
    all_clients: Option<String>,
}

impl Storable for UpdateOutcome {
    fn take_preview(&mut self) -> Option<Preview> {
        self.inner.take_preview()
    }

    fn record_plan(
        &mut self,
        plan_id: crate::model::PlanId,
        expires_at: jiff::Timestamp,
        hint: crate::error::CommandHint,
    ) {
        self.inner.record_plan(plan_id, expires_at, hint);
    }
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
        if let Some(line) = &self.all_clients {
            writeln!(w, "{line}")?;
        }
        self.inner.render_human(w, style)
    }
}

fn noun(n: usize) -> &'static str {
    if n == 1 { "addon" } else { "addons" }
}

/// `update -t all`'s first human line (M4-D13): how many clients it selected, and which
/// of them are test clients, so the wider selection is never silent.
fn all_line(targets: &[&Target]) -> String {
    if targets.is_empty() {
        return "-t all selects no client".to_owned();
    }
    let clients = crate::output::count(targets.len(), "client");
    let mut tests: Vec<&str> = targets
        .iter()
        .filter(|t| matches!(t.kind, TargetKind::Test))
        .map(|t| t.id.as_str())
        .collect();
    tests.sort_unstable();
    if tests.is_empty() {
        return format!("-t all selects {clients}, none of them a test client");
    }
    let count = crate::output::count(tests.len(), "test client");
    format!(
        "-t all selects {clients}, including {count} ({})",
        tests.join(", ")
    )
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
    let verbose = ctx.output.style.detail();
    let mut warnings = world.warnings.clone();
    let mut notes = Vec::new();
    let (catalog, catalog_warnings) = world.catalog(ctx);
    warnings.extend(catalog_warnings);
    let mut targets: Vec<&DiscoveredTarget> = world.select_many(ctx)?;
    targets.sort_by(|a, b| a.target.id.cmp(&b.target.id));
    let all_clients = world.selects_all(ctx).then(|| {
        let selected: Vec<&Target> = targets.iter().map(|d| &d.target).collect();
        all_line(&selected)
    });
    let records = world.records()?;
    let (scope, unknown) = in_scope(&targets, &records, &catalog, &args.addons);
    // A client named among the addons (M4-D12 hook 3), before anything is written.
    super::resolve_args::misplaced_clients(ctx, &world, &catalog, &args.addons, &|w| {
        targets.iter().any(|d| {
            records
                .get(&d.target.id)
                .is_some_and(|r| r.addons.values().any(|a| names_addon(w, a, &catalog)))
        })
    })?;
    // A named addon with no client to be on is `not_managed`, as on clients without it:
    // never dropped for a "nothing to update".
    if targets.is_empty()
        && let Some(name) = unknown.first()
    {
        return Err(Error::new(
            ErrorCode::NotManaged,
            format!("{name} is not installed on any client"),
            Suggestion::command(["omaddons", "install", name.as_str()]),
        ));
    }

    let engine = Engine::new(ctx, &world, "update")?;
    let enabled = engine.enabled_sources();
    // `--offline` wins over `--refresh`.
    let freshness = if ctx.offline {
        Freshness::OfflineOnly
    } else if args.refresh {
        Freshness::Refresh
    } else {
        Freshness::Window(SOURCE_RECHECK_MIN_INTERVAL)
    }
    // `apply` revalidates every source it touches (M4-D20).
    .forced(ctx.force_revalidate);
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
        &|r, c, family, newest| cache.remembered_look_back(r, c, family, newest),
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
                any_flavor: AnyFlavorIntent::Keep,
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

    // What the token covers beyond the engine's plan: what was left alone, and why.
    let skips = update_skips(&pushed);
    let extras = PlanExtras {
        update_skips: &skips,
        ..PlanExtras::default()
    };
    let run = RunId::new();
    let (done, token, plan_precondition): (
        Option<TxOutcomes>,
        Option<String>,
        Option<Precondition>,
    ) = if changes.is_empty() {
        // Every selected client: the preview said there was nothing to do on each. A
        // stale bound run changes nothing, not even a recovery.
        let idle: Vec<Idle<'_>> = targets
            .iter()
            .map(|d| Idle {
                target: &d.target,
                generation: records.get(&d.target.id).map_or(0, |r| r.generation),
            })
            .collect();
        let token = match precondition::check_noop(ctx, "update", &idle, &extras) {
            Ok(token) => token,
            Err(error) => {
                // What the statuses fetched is still worth remembering, so a re-preview
                // after a stale run sees it (M4-D20).
                cache.remember(&[]);
                return Err(error);
            }
        };
        recover_waiting(ctx, &world, "update");
        (None, token, None)
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
            // The run the token describes: a dry run's, or a bound run's to compare.
            witness: precondition::witnessing(ctx),
        };
        let binding = PlanBinding::of_run(ctx, "update", extras.clone());
        match engine.run_bound(
            &req,
            Some(&cache),
            binding.as_ref().map(|b| b as &dyn Binding),
        ) {
            Ok(done) => {
                let precondition = ctx
                    .dry_run
                    .then(|| precondition::plan_precondition("update", &done.plan, &extras));
                let token = precondition.as_ref().map(Precondition::token);
                (Some(done), token, precondition)
            }
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
    // Each unknown name's own error: an item keeps no alternatives, so one that is the
    // command's only failure is returned whole, its other clients' adopt fixes included.
    let mut unknown_errors: Vec<Error> = Vec::new();
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
            unknown_errors.push(error.clone());
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
        // ubs:ignore — comparing target ids, not secrets
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
        if let ([only], 1) = (unknown_errors.as_slice(), data.summary.failed) {
            return Err(only.clone());
        }
        return Err(error);
    }
    let locks: Vec<(TxId, u64)> = done
        .iter()
        .flat_map(|d| &d.outcomes)
        .filter_map(|o| Some((o.tx?, o.lock_held_ms?)))
        .collect();
    let selected: Vec<&Target> = targets.iter().map(|d| &d.target).collect();
    let preview = match done
        .as_ref()
        .filter(|done| done.plan.has_ops())
        .zip(plan_precondition)
    {
        Some((done, p)) => intent::preview_plan(
            ctx,
            &StorableArgs::Update {
                args,
                scope: TargetScope::of(ctx.target.as_deref(), &selected),
            },
            p,
            &done.plan,
            &records,
        )?,
        None => None,
    };
    let flags = ApprovalFlags {
        while_running: ctx.while_running,
        ..ApprovalFlags::default()
    };
    let approval = if ctx.dry_run {
        Some(match &done {
            Some(done) => Approval::of_plan(&done.plan, &records, &flags)?,
            None => Approval::nothing(&flags),
        })
    } else {
        None
    };
    let data = data
        .with_precondition(token)
        .with_approval(approval)
        .listing(verbose);
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
            preview,
        },
        strict: args.strict,
        locks,
        notes,
        all_clients,
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

/// The skip decisions made before the engine (`PlanExtras::update_skips`): each addon
/// left alone, by target, and why. Failed and unchanged items decide nothing, and the
/// `blocked` skips `settle_failures` makes later are the engine's results.
fn update_skips(
    pushed: &[(TargetRef, ItemResult)],
) -> Vec<(precondition::TargetRef, String, SkipReason)> {
    pushed
        .iter()
        .filter(|(_, i)| i.status == ItemStatus::Skipped)
        .filter_map(|(t, i)| {
            let name = i.name.clone().unwrap_or_else(|| i.request.clone());
            Some((
                precondition::TargetRef::of(t.key, &t.id),
                name,
                i.skip_reason?,
            ))
        })
        .collect()
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
                error.set_fix(Suggestion::command([
                    "omaddons".to_owned(),
                    "install".to_owned(),
                    name,
                    "--force".to_owned(),
                    "-t".to_owned(),
                    id.clone(),
                ]));
            }
        }
    }
    data.recount();
}

/// A verification refusal: the release exists but no build of it loads here (or none
/// is as new as an adopted build), nor does a release a GitHub look-back passed over
/// (code `look_back`). A download or read failure is not one. A look-back that could not
/// be made (`asset: "look_back"`) changes nothing: the newest release is refused all
/// the same.
fn blocked(error: &ErrorBody) -> bool {
    let refusal =
        |code: &str| matches!(code, "wrong_flavor" | "older_than_installed" | "look_back");
    match error.code {
        ErrorCode::WrongFlavor => true,
        ErrorCode::NoCompatibleRelease => error.details["tried"].as_array().is_some_and(|tried| {
            let judged: Vec<&serde_json::Value> =
                tried.iter().filter(|t| t["asset"] != "look_back").collect();
            !judged.is_empty()
                && judged
                    .iter()
                    .all(|t| t["code"].as_str().is_some_and(refusal))
        }),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::TargetKey;

    #[test]
    fn update_skips_are_the_skip_decisions() {
        let item = |name: &str, status: ItemStatus, skip_reason: Option<SkipReason>| ItemResult {
            request: name.to_owned(),
            name: Some(name.to_owned()),
            r#ref: None,
            status,
            skip_reason,
            duplicate_of: None,
            reason: None,
            from_version: None,
            to_version: None,
            folders: None,
            required_by: None,
            deps: ItemDeps::default(),
            error: None,
        };
        let key = TargetKey::new();
        let retail = TargetRef {
            id: "retail".into(),
            key: Some(key),
            mode: None,
        };
        // Not registered yet: named by its id.
        let era = TargetRef {
            id: "era".into(),
            key: None,
            mode: None,
        };
        let pushed = vec![
            (
                retail.clone(),
                item("Pin", ItemStatus::Skipped, Some(SkipReason::Pinned)),
            ),
            (
                retail.clone(),
                item("New", ItemStatus::Skipped, Some(SkipReason::Ahead)),
            ),
            (
                era.clone(),
                item("Old", ItemStatus::Skipped, Some(SkipReason::Blocked)),
            ),
            (
                era.clone(),
                item("Zip", ItemStatus::Skipped, Some(SkipReason::Static)),
            ),
            (
                retail.clone(),
                item("Off", ItemStatus::Skipped, Some(SkipReason::Unchecked)),
            ),
            (retail.clone(), item("Up", ItemStatus::Updated, None)),
            (era.clone(), item("Same", ItemStatus::Unchanged, None)),
            (retail, item("Gone", ItemStatus::Failed, None)),
        ];
        let skips: Vec<(String, String, serde_json::Value)> = update_skips(&pushed)
            .into_iter()
            .map(|(t, name, reason)| (t.to_string(), name, serde_json::json!(reason)))
            .collect();
        let want = |t: &str, name: &str, reason: &str| {
            (t.to_owned(), name.to_owned(), serde_json::json!(reason))
        };
        let key = key.to_string();
        assert_eq!(
            skips,
            vec![
                want(&key, "Pin", "pinned"),
                want(&key, "New", "ahead"),
                want("era", "Old", "blocked"),
                want("era", "Zip", "static"),
                want(&key, "Off", "unchecked"),
            ],
            "updated, unchanged and failed items are no skip decisions: {pushed:?}"
        );
    }

    #[test]
    fn the_all_line_counts_the_test_clients() {
        let client = |id: &str, kind: TargetKind| {
            let mut target =
                crate::engine::testing::target(&std::path::PathBuf::from(format!("/c/{id}")));
            target.id = id.into();
            target.kind = kind;
            target
        };
        let (live, test) = (TargetKind::Live, TargetKind::Test);
        for (case, clients, want) in [
            (
                "three_with_one_test",
                vec![
                    client("retail", live),
                    client("ptr", test),
                    client("era", live),
                ],
                "-t all selects 3 clients, including 1 test client (ptr)",
            ),
            (
                "six_with_two_tests",
                vec![
                    client("retail", live),
                    client("ptr", test),
                    client("era", live),
                    client("classic", live),
                    client("classic-ptr", test),
                    client("anniversary", live),
                ],
                "-t all selects 6 clients, including 2 test clients (classic-ptr, ptr)",
            ),
            (
                "two_live",
                vec![client("retail", live), client("era", live)],
                "-t all selects 2 clients, none of them a test client",
            ),
            (
                "one_live",
                vec![client("retail", live)],
                "-t all selects 1 client, none of them a test client",
            ),
            ("none", Vec::new(), "-t all selects no client"),
        ] {
            let targets: Vec<&Target> = clients.iter().collect();
            assert_eq!(all_line(&targets), want, "{case}");
        }
    }

    #[test]
    fn only_verification_refusals_are_blocked() {
        let body = |code: ErrorCode, tried: serde_json::Value| {
            ErrorBody::from(
                Error::new(code, "x", Suggestion::text("y"))
                    .details(serde_json::json!({ "tried": tried })),
            )
        };
        let reasons = |r: &[(&str, &str)]| -> serde_json::Value {
            r.iter()
                .map(|(code, message)| {
                    serde_json::json!({"asset": "a.zip", "code": code, "message": message})
                })
                .collect()
        };
        assert!(blocked(&body(
            ErrorCode::WrongFlavor,
            serde_json::json!(null)
        )));
        assert!(blocked(&body(
            ErrorCode::NoCompatibleRelease,
            reasons(&[("wrong_flavor", "Foo: …"), ("older_than_installed", "…")])
        )));
        assert!(!blocked(&body(
            ErrorCode::NoCompatibleRelease,
            reasons(&[("wrong_flavor", "…"), ("network", "connection reset")])
        )));
        // A look-back that passed over each older release, and one that failed.
        assert!(blocked(&body(
            ErrorCode::NoCompatibleRelease,
            reasons(&[
                ("wrong_flavor", "…"),
                (
                    "look_back",
                    "its release.json declares no build for this client"
                )
            ])
        )));
        let failed_look_back = serde_json::json!([
            {"asset": "Foo.zip", "code": "wrong_flavor", "message": "…"},
            {"asset": "look_back", "code": "rate_limited", "message": "GitHub's API allows 5 more requests"},
        ]);
        assert!(blocked(&body(
            ErrorCode::NoCompatibleRelease,
            failed_look_back
        )));
        let only_look_back = serde_json::json!([
            {"asset": "look_back", "code": "network", "message": "connection reset"},
        ]);
        assert!(!blocked(&body(
            ErrorCode::NoCompatibleRelease,
            only_look_back
        )));
        assert!(!blocked(&body(
            ErrorCode::NoCompatibleRelease,
            reasons(&[])
        )));
        assert!(!blocked(&body(
            ErrorCode::Network,
            reasons(&[("wrong_flavor", "…")])
        )));
    }
}
