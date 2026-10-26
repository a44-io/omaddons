//! `omaddons pin <addon> [--version V] [-t T]` and `omaddons unpin <addon> [-t T]`: hold
//! an addon at its release through `update`, and let it go again (M2-D5).
//!
//! Pin and unpin are records-only transactions of kind `pin`: no game file changes, so
//! they run while the client runs, and like every change they are journaled,
//! snapshotted and undoable. Without `-t` they act on every target where the addon is
//! installed, one transaction per target under one run. `pin --version V` installs V
//! first, through the install pipeline, then pins it. An addon already pinned (or
//! unpinned) is `unchanged`, and its target gets no transaction.
//! `--dry-run` returns `data.precondition`, and `--if-match <it>` pins exactly what
//! that preview showed or nothing (M4-D19); a bound no-op still checks its token.

use std::collections::BTreeMap;

use super::adopt::hints::{adopt_fixes, adopting};
use super::engine::{Engine, recover_waiting};
use super::outcome::{
    ItemDeps, ItemRequest, ItemResult, ItemStatus, TargetRef, TargetsData, TargetsOutcome,
};
use super::precondition::{self, Idle, PlanBinding, PlanExtras};
use super::world::{World, names_addon};
use crate::cli::Context;
use crate::discover::DiscoveredTarget;
use crate::engine::edit::RecordEdit;
use crate::engine::plan::{AnyFlavorIntent, PinIntent, RequestedChange};
use crate::engine::{TxRequest, TxTarget};
use crate::error::{CommandHint, Error, ErrorCode, Result, Suggestion, Warning};
use crate::model::{AddonRecord, Ref, RunId, TxId, TxKind};
use crate::output::{Outcome, Style};
use crate::resolve::cache::Freshness;
use crate::resolve::updates::{UpdateStatus, evaluate, update_floor};
use crate::state::memo::{self, FactIndex};
use crate::state::records::TargetRecord;

/// `pin`'s own arguments.
#[derive(Debug, Clone, Default, clap::Args)]
pub struct PinArgs {
    /// The installed addon: its name, or a catalog name or alias
    #[arg(value_name = "ADDON")]
    pub addon: String,
    /// Install this release first (a tag for gh: refs, a rev for git: refs), then pin it
    #[arg(long, value_name = "VERSION")]
    pub version: Option<String>,
}

/// `unpin`'s own arguments.
#[derive(Debug, Clone, Default, clap::Args)]
pub struct UnpinArgs {
    /// The installed addon: its name, or a catalog name or alias
    #[arg(value_name = "ADDON")]
    pub addon: String,
}

/// What the command does.
#[derive(Debug, Clone, Copy)]
enum Mode<'a> {
    Pin { version: Option<&'a str> },
    Unpin,
}

impl Mode<'_> {
    const fn command(self) -> &'static str {
        match self {
            Self::Pin { .. } => "pin",
            Self::Unpin => "unpin",
        }
    }

    /// What a changed item is called.
    const fn status(self) -> ItemStatus {
        match self {
            Self::Pin { .. } => ItemStatus::Pinned,
            Self::Unpin => ItemStatus::Unpinned,
        }
    }
}

/// `pin`'s and `unpin`'s outcome: the shared multi-target outcome.
#[derive(Debug)]
pub struct PinOutcome {
    inner: TargetsOutcome,
    /// Each committed transaction's global lock hold time, for `-v` log lines.
    pub locks: Vec<(TxId, u64)>,
    /// For `-v` log lines.
    pub notes: Vec<String>,
}

impl Outcome for PinOutcome {
    type Data = TargetsData;

    fn command(&self) -> &'static str {
        self.inner.command
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

    fn render_human(&self, w: &mut dyn std::io::Write, style: &Style) -> std::io::Result<()> {
        self.inner.render_human(w, style)
    }
}

/// Runs `pin`.
pub fn pin(ctx: &Context, args: &PinArgs) -> Result<PinOutcome> {
    run(
        ctx,
        &args.addon,
        Mode::Pin {
            version: args.version.as_deref(),
        },
    )
}

/// Runs `unpin`.
pub fn unpin(ctx: &Context, args: &UnpinArgs) -> Result<PinOutcome> {
    run(ctx, &args.addon, Mode::Unpin)
}

fn run(ctx: &Context, addon: &str, mode: Mode<'_>) -> Result<PinOutcome> {
    let command = mode.command();
    let world = World::load(ctx)?;
    let verbose = ctx.output.style.verbose > 0;
    let mut warnings = world.warnings.clone();
    let mut notes = Vec::new();
    let (catalog, catalog_warnings) = world.catalog(ctx);
    warnings.extend(catalog_warnings);
    let mut targets: Vec<&DiscoveredTarget> = world.select_many(ctx)?;
    targets.sort_by(|a, b| a.target.id.cmp(&b.target.id));
    let records = world.records()?;
    // Only the targets where the addon is installed.
    let found: Vec<(&DiscoveredTarget, &AddonRecord)> = targets
        .iter()
        .filter_map(|d| {
            let record = records.get(&d.target.id)?;
            let a = record
                .addons
                .values()
                .find(|a| names_addon(addon, a, &catalog))?;
            Some((*d, a))
        })
        .collect();
    if found.is_empty() {
        let checked: Vec<&str> = targets.iter().map(|d| d.target.id.as_str()).collect();
        let error = Error::new(
            ErrorCode::NotManaged,
            format!("{addon} is not installed on {}", checked.join(", ")),
            Suggestion::command(["omaddons", "install", addon]),
        );
        let fixes = adopt_fixes(&world, &records, &targets, addon);
        return Err(adopting(error, fixes));
    }
    if let Mode::Pin {
        version: Some(_), ..
    } = mode
        && let Some((_, a)) = found
            .iter()
            .find(|(_, a)| matches!(a.r#ref, Ref::Url { .. } | Ref::File { .. }))
    {
        return Err(Error::new(
            ErrorCode::Usage,
            format!(
                "--version picks a release of a gh: or git: ref; {} comes from {}",
                a.name, a.r#ref
            ),
            Suggestion::command(["omaddons", "pin", addon]),
        ));
    }

    let mut changes: BTreeMap<String, Vec<RequestedChange>> = BTreeMap::new();
    let mut requested = Vec::new();
    let mut unchanged = Vec::new();
    for (d, a) in &found {
        let change = match mode {
            Mode::Pin { version: None } => (!a.pinned).then(|| RequestedChange::Edit {
                addon: a.name.clone(),
                edit: RecordEdit::SetPinned(true),
            }),
            Mode::Unpin => a.pinned.then(|| RequestedChange::Edit {
                addon: a.name.clone(),
                edit: RecordEdit::SetPinned(false),
            }),
            Mode::Pin {
                version: Some(version),
            } => Some(RequestedChange::Install {
                r#ref: a.r#ref.clone(),
                channel: a.channel,
                channel_explicit: false,
                version: Some(version.to_owned()),
                file: None,
                name: Some(a.name.clone()),
                pin: PinIntent::Pin,
                reason: a.reason,
                reinstall: false,
                blob_sha256: None,
                asset: None,
                revision: None,
                any_flavor: AnyFlavorIntent::Keep,
            }),
        };
        match change {
            Some(change) => {
                changes.entry(d.target.id.clone()).or_default().push(change);
                requested.push(ItemRequest {
                    request: addon.to_owned(),
                    target: d.target.id.clone(),
                    addon: Some(a.name.clone()),
                    r#ref: Some(a.r#ref.clone()),
                    version: Some(a.version.clone()),
                });
            }
            None => unchanged.push((
                target_ref(d),
                ItemResult {
                    request: addon.to_owned(),
                    name: Some(a.name.clone()),
                    r#ref: Some(a.r#ref.clone()),
                    status: ItemStatus::Unchanged,
                    skip_reason: None,
                    duplicate_of: None,
                    reason: None,
                    from_version: Some(a.version.clone()),
                    to_version: Some(a.version.clone()),
                    folders: None,
                    required_by: None,
                    deps: ItemDeps::default(),
                    error: None,
                },
            )),
        }
    }

    let run = RunId::new();
    let records_only = !matches!(mode, Mode::Pin { version: Some(_) });
    let mut data = TargetsData::assemble(&[], &[], &[], ctx.dry_run, run, &[]);
    let mut locks = Vec::new();
    let token;
    if !changes.is_empty() {
        let engine = Engine::new(ctx, &world, command)?;
        let freshness = if ctx.offline {
            Freshness::OfflineOnly
        } else {
            Freshness::Revalidate
        };
        let cache = engine.cache(freshness);
        let req = TxRequest {
            targets: targets
                .iter()
                .filter_map(|d| {
                    Some(TxTarget {
                        discovered: (*d).clone(),
                        changes: changes.remove(&d.target.id)?,
                        default_kind: if records_only {
                            TxKind::Pin
                        } else {
                            TxKind::Install
                        },
                        dependency_locks: BTreeMap::new(),
                    })
                })
                .collect(),
            force: false,
            while_running: ctx.while_running,
            dry_run: ctx.dry_run,
            reason: ctx.reason.clone(),
            kind: records_only.then_some(TxKind::Pin),
            run,
            no_deps: false,
            witness: precondition::witnessing(ctx),
        };
        let extras = PlanExtras::default();
        let binding = PlanBinding::of_run(ctx, command, extras.clone());
        let done = engine.run_bound(
            &req,
            Some(&cache),
            binding.as_ref().map(|b| b as &dyn crate::engine::Binding),
        );
        let remembered = cache.remember(done.as_ref().map_or(&[][..], |d| &d.plan.facts));
        let done = done?;
        token = ctx
            .dry_run
            .then(|| precondition::plan_token(command, &done.plan, &extras));
        warnings.extend(remembered.warnings);
        notes.extend(remembered.notes);
        data = TargetsData::build(&done, run, &requested);
        for o in &done.outcomes {
            warnings.extend(o.warnings.iter().cloned());
            notes.extend(o.notes.iter().cloned());
            if let (Some(tx), Some(ms)) = (o.tx, o.lock_held_ms) {
                locks.push((tx, ms));
            }
        }
        warnings.extend(done.warnings.iter().cloned());
        // Whatever it asked for and moved, the command pinned (or unpinned); a dependency
        // the new version brought in is installed, not pinned.
        for t in &mut data.targets {
            let id = t.target.id.as_str();
            for item in &mut t.items {
                let asked = requested
                    .iter()
                    .any(|r| r.target == id && r.request == item.request);
                if asked && item.status.changed() {
                    item.status = mode.status();
                }
            }
        }
        data.recount();
    } else {
        // A stale bound no-op must refuse before it can recover any journal.
        token = precondition::check_noop(
            ctx,
            command,
            &noop_targets(&found, &records),
            &PlanExtras::default(),
        )?;
        recover_waiting(ctx, &world, command);
    }
    for (target, item) in unchanged {
        data.push_item(target, item);
    }
    for t in &data.targets {
        for i in t.items.iter().filter(|i| i.status == ItemStatus::Failed) {
            if let Some(e) = &i.error {
                let mut warning = Warning::item(&e.error());
                warning.message = format!("{}: {}", t.target.id, e.message);
                warnings.push(warning);
            }
        }
    }
    if let Some(error) = data.all_failed() {
        return Err(error);
    }
    let hints = match mode {
        Mode::Unpin => unpin_hints(&world, &found, &data),
        Mode::Pin { .. } => Vec::new(),
    };
    data.precondition = token;
    let data = data.listing(verbose);
    let commands = data.undo_commands();
    let mut unique: Vec<Warning> = Vec::new();
    for w in warnings {
        if !unique.contains(&w) {
            unique.push(w);
        }
    }
    Ok(PinOutcome {
        inner: TargetsOutcome {
            command,
            data,
            warnings: unique,
            commands,
            notes: hints,
        },
        locks,
        notes,
    })
}

/// The clients examined to decide that the installed addon needs no record change.
fn noop_targets<'a>(
    found: &[(&'a DiscoveredTarget, &AddonRecord)],
    records: &BTreeMap<String, TargetRecord>,
) -> Vec<Idle<'a>> {
    found
        .iter()
        .map(|(d, _)| Idle {
            target: &d.target,
            generation: records.get(&d.target.id).map_or(0, |r| r.generation),
        })
        .collect()
}

fn target_ref(d: &DiscoveredTarget) -> TargetRef {
    TargetRef {
        id: d.target.id.clone(),
        key: d.target.key,
        mode: None,
    }
}

/// What `update` would do with each addon just unpinned, from the release memo and the
/// shared evaluator: no request.
fn unpin_hints(
    world: &World,
    found: &[(&DiscoveredTarget, &AddonRecord)],
    data: &TargetsData,
) -> Vec<String> {
    let (memo, _) = memo::load(&world.paths);
    let facts = FactIndex::new(&memo.candidates);
    let mut hints = Vec::new();
    for (d, a) in found {
        let unpinned = data.targets.iter().any(|t| {
            t.target.id == d.target.id && t.items.iter().any(|i| i.status == ItemStatus::Unpinned)
        });
        if !unpinned {
            continue;
        }
        let mut record = (*a).clone();
        record.pinned = false;
        let entry = memo.entries.get(&memo::key(&a.r#ref, a.channel));
        let floor = update_floor(&d.target, a, &world.table);
        let id = &d.target.id;
        let name = &a.name;
        let look_back = |newest: &str| {
            let tag = newest.rsplit_once('@').map(|(_, tag)| tag)?;
            memo.look_back(&a.r#ref, a.channel, d.target.family, tag)
        };
        hints.push(
            match evaluate(&record, entry, &facts, &d.target, floor, true, &look_back) {
                UpdateStatus::UpdateAvailable { latest, .. } => format!(
                    "{id}: `omaddons update {name} -t {id}` installs {}",
                    latest.version
                ),
                UpdateStatus::Current { .. } => format!("{id}: {name} is current"),
                _ => format!("{id}: `omaddons outdated -t {id}` checks {name} for updates"),
            },
        );
    }
    hints
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::testing::TxScene;
    use crate::engine::witness::WitnessScope;

    fn scene_with_foo() -> TxScene {
        let scene = TxScene::new();
        let zip = scene.zip("foo.zip", &[("Foo/Foo.toc", "## Interface: 120105\n")]);
        let done = scene.run(vec![scene.install("gh:o/foo", &zip)]).unwrap();
        assert!(done.failures.is_empty(), "install Foo: {:?}", done.failures);
        scene
    }

    #[test]
    fn the_noop_scope_is_every_target_holding_the_addon() {
        let scene = scene_with_foo();
        let retail = scene.current_target();
        let mut era = retail.clone();
        era.target.id = "era".into();
        let mut retail_record = scene.record();
        retail_record.generation = 3;
        let mut era_record = retail_record.clone();
        era_record.generation = 7;
        let addon = &retail_record.addons["foo"];
        for (case, records, found, expected) in [
            (
                "two targets in found order",
                BTreeMap::from([
                    ("retail".into(), retail_record.clone()),
                    ("era".into(), era_record),
                    ("unselected".into(), retail_record.clone()),
                ]),
                vec![(&retail, addon), (&era, addon)],
                vec![("retail", 3), ("era", 7)],
            ),
            (
                "a found target without a record",
                BTreeMap::new(),
                vec![(&retail, addon)],
                vec![("retail", 0)],
            ),
        ] {
            let actual = noop_targets(&found, &records);
            let actual: Vec<_> = actual
                .iter()
                .map(|i| (i.target.id.as_str(), i.generation))
                .collect();
            assert_eq!(
                actual, expected,
                "{case}: found={found:?}, records={records:?}"
            );
        }
    }

    #[test]
    fn a_records_only_pin_and_its_noop_differ_in_selection_only() {
        for (command, initially_pinned, pinned) in [("pin", false, true), ("unpin", true, false)] {
            let scene = scene_with_foo();
            if initially_pinned {
                scene
                    .run_as(
                        vec![scene.edit("Foo", RecordEdit::SetPinned(true))],
                        Some(TxKind::Pin),
                        TxKind::Pin,
                    )
                    .unwrap();
            }
            let done = scene
                .run_bound(
                    vec![scene.edit("Foo", RecordEdit::SetPinned(pinned))],
                    Some(TxKind::Pin),
                    TxKind::Pin,
                    true,
                    Some(WitnessScope::default()),
                    None,
                )
                .unwrap();
            assert!(done.failures.is_empty(), "{command}: {:?}", done.failures);
            let target = scene.current_target();
            let record = scene.record();
            let extras = PlanExtras::default();
            let planned = precondition::of_plan(command, &done.plan, &done.plan.witnesses, &extras);
            let idle = precondition::of_noop(
                command,
                &[Idle {
                    target: &target.target,
                    generation: record.generation,
                }],
                &extras,
            )
            .unwrap();
            let planned_parts = planned.parts();
            let idle_parts = idle.parts();
            let differs = std::array::from_fn::<_, 4, _>(|i| planned_parts[i] != idle_parts[i]);
            assert_eq!(
                differs,
                [false, false, true, false],
                "{command}: generation={}, pin={pinned}, planned={planned_parts:?}, idle={idle_parts:?}",
                record.generation
            );
        }
    }
}
