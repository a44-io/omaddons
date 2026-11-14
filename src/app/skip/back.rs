//! `omaddons skip <addon> --back [--tx T | --to V] [-t T]`: go back one addon to the
//! release before the installed one, and pass over the one it leaves (regression
//! guard, design 7.5.4, RG-34c).
//!
//! The exact forms go back along one transition of history (7.2.1): `--tx T`, or the
//! newest line that moved the addon to the release installed now. Its `to` must be what
//! is installed (else `plan_stale`), and its `from`, with the witness recorded then, is
//! what goes back in: a locked install from the blob cache while the archive is there,
//! else from the source held to that witness, never a label. `--to V` is the label form:
//! the release the source serves as V now, said in so many words. Either way the change
//! is an update that keeps the pin and marks the release it leaves (`origin: rollback`)
//! in the same commit, and the engine checks both again under the lock (RG-34b).

use std::collections::BTreeMap;
use std::io::Write;

use super::{SkipArgs, SkipOutcome};
use crate::app::adopt::hints::{adopt_fixes, adopting};
use crate::app::approval::{Approval, ApprovalFlags};
use crate::app::engine::Engine;
use crate::app::history::kind_name;
use crate::app::history_items::{Reader, transitions_of};
use crate::app::intent::{self, StorableArgs, TargetScope};
use crate::app::outcome::{ItemRequest, ItemStatus, TargetsData, TargetsOutcome};
use crate::app::precondition::{self, PlanBinding, PlanExtras};
use crate::app::world::{World, names_addon};
use crate::cli::Context;
use crate::contract::actions::Action;
use crate::discover::DiscoveredTarget;
use crate::engine::blobs::BlobCache;
use crate::engine::plan::{
    AnyFlavorIntent, PinIntent, RequestedChange, Restorable, RollbackOf, restorable,
};
use crate::engine::{TxRequest, TxTarget};
use crate::error::{
    CommandHint, Error, ErrorCode, PlanStaleSource, Result, StalePart, Suggestion, Warning,
};
use crate::model::{
    AddonRecord, InstalledRelease, Ref, ReleaseTransition, RunId, SkippedNote, TxId, TxKind,
    skip_label,
};
use crate::output::Style;
use crate::resolve::cache::Freshness;
use crate::resolve::updates::skip_covering;
use crate::sources::SourceId;
use crate::state::history::{HistoryEntry, HistoryFilter};

/// What one client's rollback did, for its human lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct BackLine {
    target: String,
    addon: String,
    /// The release it leaves, and the one it goes back to.
    left: String,
    back: String,
    /// Where the files came from.
    from: Source,
    tx: Option<TxId>,
    dry_run: bool,
}

/// Where a rollback's files come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    /// The archive installed then, from the blob cache.
    Cache,
    /// The archive installed then, from its source, held to its recorded witness.
    Verified(SourceId),
    /// `--to`: what the source serves under that label now.
    Label(SourceId),
}

/// The lines of one client's rollback (design 5.1).
pub(super) fn render(w: &mut dyn Write, line: &BackLine) -> std::io::Result<()> {
    let BackLine {
        target,
        addon,
        left,
        back,
        from,
        tx,
        dry_run,
    } = line;
    let source = match from {
        Source::Cache => "the files installed before, from the local cache".to_owned(),
        Source::Verified(s) => format!(
            "the files installed before, from {}, checked against their recorded hash",
            s.name()
        ),
        Source::Label(s) => format!("{back} as {} serves it now", s.name()),
    };
    let would = if *dry_run { " (preview)" } else { "" };
    writeln!(w, "{addon} {left} → {back} on {target}{would} · {source}")?;
    if *dry_run {
        writeln!(
            w,
            "{left} would be skipped: update would pass over it and resume at the next release"
        )?;
    } else {
        writeln!(
            w,
            "{left} is skipped: update passes over it and resumes at the next release"
        )?;
    }
    if let Some(tx) = tx {
        writeln!(w, "undo: omaddons undo {tx}")?;
        writeln!(
            w,
            "offer {left} again: omaddons unskip --tx {tx} -t {target}"
        )?;
    }
    Ok(())
}

/// `--tx` as a transaction id; `usage` for one that is not a full id.
fn tx_of(args: &SkipArgs) -> Result<Option<TxId>> {
    args.tx
        .as_deref()
        .map(|tx| {
            tx.parse::<TxId>().map_err(|_| {
                Error::new(
                    ErrorCode::Usage,
                    format!(
                        "--tx {tx} is not a transaction id: give its full id, as history shows it"
                    ),
                    Suggestion::command(["omaddons", "history"]),
                )
            })
        })
        .transpose()
}

/// The releases listing an earlier version can be named from.
fn releases_fix(addon: &str, target: &str) -> Suggestion {
    Suggestion::command(["omaddons", "info", addon, "--releases", "-t", target])
}

/// `not_found`: there is no exact release to go back to from here; an earlier version
/// has to be named with `--to`.
fn name_one(message: String, addon: &str, target: &str) -> Error {
    Error::new(
        ErrorCode::NotFound,
        format!(
            "{message}; name an earlier version with --to, which installs what the source serves under that label now"
        ),
        releases_fix(addon, target),
    )
}

/// The transition of `entry` for `addon` (by record name), with the line's source.
fn transition_for(
    entry: &HistoryEntry,
    reader: &mut Reader<'_>,
    addon: &AddonRecord,
) -> Option<ReleaseTransition> {
    let (transitions, _) = transitions_of(entry, reader);
    transitions
        .into_iter()
        .find(|t| t.addon.eq_ignore_ascii_case(&addon.name))
}

/// The newest line on the client keyed `key` that moved `addon` to the release
/// installed now: a records-only change since (a pin) rewrites the record's `tx_id`
/// but moves no release.
fn installing_line(
    entries: &[HistoryEntry],
    reader: &mut Reader<'_>,
    d: &DiscoveredTarget,
    addon: &AddonRecord,
) -> Option<usize> {
    let installed = InstalledRelease::of(addon);
    (0..entries.len()).rev().find(|&i| {
        let e = &entries[i];
        e.target.is_some() && e.target == d.target.key && {
            transition_for(e, reader, addon).is_some_and(|t| {
                t.to.as_ref().is_some_and(|to| to.same_artifact(&installed))
                    && t.from
                        .as_ref()
                        .is_none_or(|from| !from.same_artifact(&installed))
            })
        }
    })
}

/// `plan_stale`: the addon is no longer at the release the transition installed.
fn moved_on(
    a: &AddonRecord,
    to: Option<&InstalledRelease>,
    entry: &HistoryEntry,
    target: &str,
) -> Error {
    let message = match to {
        Some(to) => format!(
            "{} is at {}, not at {}, the release that {} installed",
            a.name,
            a.version,
            to.version,
            kind_name(entry.kind)
        ),
        None => format!(
            "{} {} took {} away; it is installed again since",
            kind_name(entry.kind),
            entry.tx,
            a.name
        ),
    };
    Error::plan_stale(
        PlanStaleSource::Moved,
        &[StalePart::Targets],
        message,
        Suggestion::command(["omaddons", "errors", "-t", target]),
    )
}

/// The exact form: the install of `from` that goes back along `entries[at]`'s
/// transition `t` for `a`, or why it cannot.
fn exact(
    engine: &Engine,
    blobs: &BlobCache,
    entries: &[HistoryEntry],
    at: usize,
    t: &ReleaseTransition,
    d: &DiscoveredTarget,
    a: &AddonRecord,
) -> Result<(RequestedChange, Source, String)> {
    let id = d.target.id.as_str();
    let entry = &entries[at];
    let installed = InstalledRelease::of(a);
    if !t.to.as_ref().is_some_and(|to| to.same_artifact(&installed)) {
        return Err(moved_on(a, t.to.as_ref(), entry, id));
    }
    let Some(from) = &t.from else {
        return Err(name_one(
            format!(
                "{} {} first installed {}: there is no release before it to go back to",
                kind_name(entry.kind),
                entry.tx,
                a.name
            ),
            &a.name,
            id,
        ));
    };
    if skip_covering(a, from.release.as_ref(), &from.version).is_some() {
        return Err(name_one(
            format!(
                "{} {}, the release before this one, is already skipped",
                a.name, from.version
            ),
            &a.name,
            id,
        ));
    }
    let source = match restorable(&a.r#ref, from, blobs) {
        Restorable::Cached => Source::Cache,
        Restorable::Fetchable => Source::Verified(SourceId::of(&a.r#ref)),
        Restorable::No => {
            let fix = if engine.undoable_now(&d.target.client_dir, entries, at) {
                Suggestion::command([
                    "omaddons".to_owned(),
                    "undo".to_owned(),
                    entry.tx.to_string(),
                ])
            } else {
                releases_fix(&a.name, id)
            };
            return Err(Error::new(
                ErrorCode::NotFound,
                format!(
                    "the files of {} {} are no longer here: its archive is not in the local cache, and {} cannot serve those exact files again; --to {} would install what the source serves under that label now, which may not be them",
                    a.name,
                    from.version,
                    SourceId::of(&a.r#ref).name(),
                    from.version
                ),
                fix,
            ));
        }
    };
    let change = RequestedChange::Install {
        r#ref: a.r#ref.clone(),
        channel: a.channel,
        channel_explicit: false,
        version: Some(match (&a.r#ref, &from.witness.commit) {
            // A git release is asked for by its commit.
            (Ref::Git { .. }, Some(commit)) => commit.clone(),
            _ => from.version.clone(),
        }),
        file: None,
        name: Some(a.name.clone()),
        pin: PinIntent::Keep,
        reason: a.reason,
        reinstall: false,
        blob_sha256: from.witness.blob_sha256.clone(),
        asset: None,
        revision: from.release.as_ref().and_then(|k| k.revision),
        any_flavor: AnyFlavorIntent::Keep,
        rollback: Some(Box::new(RollbackOf {
            expect: Some(installed),
            restore_release_id: from
                .release
                .as_ref()
                .map(|k| format!("{}@{}", k.source, k.tag)),
        })),
    };
    Ok((change, source, from.version.clone()))
}

/// The label form (`--to V`): the release the source serves as V now.
fn label(to: &str, a: &AddonRecord, id: &str) -> Result<RequestedChange> {
    if matches!(a.r#ref, Ref::Url { .. } | Ref::File { .. }) {
        return Err(Error::new(
            ErrorCode::Usage,
            format!(
                "--to picks a release of the addon's source; {} comes from {}, which has no other releases",
                a.name, a.r#ref
            ),
            Suggestion::command(["omaddons", "help", "skip"]),
        ));
    }
    if skip_label(to) == skip_label(&a.version) {
        return Err(Error::new(
            ErrorCode::Usage,
            format!(
                "{} is at {} already: --to names an earlier version",
                a.name, a.version
            ),
            releases_fix(&a.name, id),
        ));
    }
    let keyed = crate::model::ReleaseKey::of_record(a).is_some();
    Ok(RequestedChange::Install {
        r#ref: a.r#ref.clone(),
        channel: a.channel,
        channel_explicit: false,
        version: Some(to.to_owned()),
        file: None,
        name: Some(a.name.clone()),
        pin: PinIntent::Keep,
        reason: a.reason,
        reinstall: false,
        blob_sha256: None,
        asset: None,
        revision: None,
        any_flavor: AnyFlavorIntent::Keep,
        rollback: Some(Box::new(RollbackOf {
            expect: keyed.then(|| InstalledRelease::of(a)),
            restore_release_id: None,
        })),
    })
}

/// `target_ambiguous`: the addon is on several selected clients and nothing names one.
fn ambiguous(ctx: &Context, addon: &str, ids: &[&str]) -> Error {
    let with = |id: &str| {
        let mut argv = ctx.argv.clone();
        argv.extend(["-t".to_owned(), id.to_owned()]);
        argv
    };
    let mut error = Error::new(
        ErrorCode::TargetAmbiguous,
        format!(
            "{addon} is installed on {} clients: {}; --back goes back on one",
            ids.len(),
            ids.join(", ")
        ),
        Suggestion::command(with(ids[0])),
    )
    .details(serde_json::json!({ "candidates": ids }));
    for id in &ids[1..] {
        error = error.alternative(Suggestion::command(with(id)));
    }
    error
}

/// Runs `skip <addon> --back`.
pub(super) fn run(ctx: &Context, args: &SkipArgs) -> Result<SkipOutcome> {
    let command = "skip";
    let [addon] = args.addons.as_slice() else {
        return Err(Error::new(
            ErrorCode::Usage,
            "--back goes back on one addon; name exactly one",
            Suggestion::command(["omaddons", "help", "skip"]),
        ));
    };
    let named_tx = tx_of(args)?;
    let world = World::load(ctx)?;
    let verbose = ctx.output.style.detail();
    let mut warnings = world.warnings.clone();
    let mut notes = Vec::new();
    let (catalog, catalog_warnings) = world.catalog(ctx);
    warnings.extend(catalog_warnings);
    let mut targets: Vec<&DiscoveredTarget> = world.select_many(ctx)?;
    targets.sort_by(|a, b| a.target.id.cmp(&b.target.id));
    let records = world.records()?;
    let found: Vec<(&DiscoveredTarget, &AddonRecord)> = targets
        .iter()
        .filter_map(|d| {
            let a = records
                .get(&d.target.id)?
                .addons
                .values()
                .find(|a| names_addon(addon, a, &catalog))?;
            Some((*d, a))
        })
        .collect();
    let not_installed = || {
        let checked: Vec<&str> = targets.iter().map(|d| d.target.id.as_str()).collect();
        adopting(
            Error::new(
                ErrorCode::NotManaged,
                format!("{addon} is not installed on {}", checked.join(", ")),
                Suggestion::command(["omaddons", "install", addon]),
            ),
            adopt_fixes(&world, &records, &targets, addon),
        )
    };
    let history = crate::state::history::read(&world.paths, &HistoryFilter::default())?;
    let entries = &history.entries;
    // The client: the one `--tx` changed, else the one client the addon is on.
    let tx_at = match named_tx {
        Some(tx) => Some(entries.iter().position(|e| e.tx == tx).ok_or_else(|| {
            Error::new(
                ErrorCode::NotFound,
                format!("there is no transaction {tx} in the history"),
                Suggestion::command(["omaddons", "history"]),
            )
        })?),
        None => None,
    };
    let (d, a) = match tx_at {
        Some(at) => {
            let key = entries[at].target;
            found
                .iter()
                .copied()
                .find(|(d, _)| key.is_some() && d.target.key == key)
                .ok_or_else(|| {
                    match targets
                        .iter()
                        .find(|d| key.is_some() && d.target.key == key)
                    {
                        Some(_) => not_installed(),
                        None => Error::new(
                            ErrorCode::NotFound,
                            format!(
                                "transaction {} changed a client that is not selected here",
                                entries[at].tx
                            ),
                            Suggestion::command(["omaddons", "history"]),
                        ),
                    }
                })?
        }
        None => match found.as_slice() {
            [] => return Err(not_installed()),
            [one] => *one,
            many => {
                let ids: Vec<&str> = many.iter().map(|(d, _)| d.target.id.as_str()).collect();
                return Err(ambiguous(ctx, addon, &ids));
            }
        },
    };
    let id = d.target.id.clone();
    let engine = Engine::new(ctx, &world, command)?;
    let blobs = BlobCache::new(&world.paths);
    // The change, where its files come from, the version it goes back to, and the
    // history line it goes back along (the exact forms).
    let (change, source, back, along) = match &args.to {
        Some(to) => (
            label(to, a, &id)?,
            Source::Label(SourceId::of(&a.r#ref)),
            to.clone(),
            None,
        ),
        None => {
            let mut reader = Reader::new(&world.paths, entries);
            let at = match tx_at {
                Some(at) => at,
                None => installing_line(entries, &mut reader, d, a).ok_or_else(|| {
                    name_one(
                        format!("no line of history moved {} to {}", a.name, a.version),
                        &a.name,
                        &id,
                    )
                })?,
            };
            let Some(t) = transition_for(&entries[at], &mut reader, a) else {
                return Err(name_one(
                    format!(
                        "{} {} recorded no release change of {} that is still known",
                        kind_name(entries[at].kind),
                        entries[at].tx,
                        a.name
                    ),
                    &a.name,
                    &id,
                ));
            };
            let (change, source, back) = exact(&engine, &blobs, entries, at, &t, d, a)?;
            (change, source, back, Some(at))
        }
    };

    let run = RunId::new();
    let requested = vec![ItemRequest {
        request: addon.clone(),
        target: id.clone(),
        addon: Some(a.name.clone()),
        r#ref: Some(a.r#ref.clone()),
        version: Some(a.version.clone()),
    }];
    let flags = ApprovalFlags {
        while_running: ctx.while_running,
        ..ApprovalFlags::default()
    };
    let freshness = if ctx.offline {
        Freshness::OfflineOnly
    } else {
        Freshness::Revalidate
    }
    // `apply` revalidates every source it touches (M4-D20).
    .forced(ctx.force_revalidate);
    let cache = engine.cache(freshness);
    let req = TxRequest {
        targets: vec![TxTarget {
            discovered: d.clone(),
            changes: vec![change],
            default_kind: TxKind::Update,
            dependency_locks: BTreeMap::new(),
        }],
        force: false,
        while_running: ctx.while_running,
        dry_run: ctx.dry_run,
        reason: ctx.reason.clone(),
        kind: Some(TxKind::Update),
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
    let plan_precondition = ctx
        .dry_run
        .then(|| precondition::plan_precondition(command, &done.plan, &extras));
    let token = plan_precondition.as_ref().map(|p| p.token());
    warnings.extend(remembered.warnings);
    notes.extend(remembered.notes);
    let mut data = TargetsData::build(&done, run, &requested);
    let mut preview = None;
    if ctx.dry_run {
        data = data.with_approval(Some(Approval::of_plan(&done.plan, &records, &flags)?));
        if let Some(p) = plan_precondition.filter(|_| done.plan.has_ops()) {
            let selected = [&d.target];
            let scope = TargetScope::of(ctx.target.as_deref(), &selected);
            let stored = StorableArgs::Skip { args, scope };
            preview = intent::preview_plan(ctx, &stored, p, &done.plan, &records)?;
        }
    }
    let mut locks = Vec::new();
    for o in &done.outcomes {
        warnings.extend(o.warnings.iter().cloned());
        notes.extend(o.notes.iter().cloned());
        if let (Some(tx), Some(ms)) = (o.tx, o.lock_held_ms) {
            locks.push((tx, ms));
        }
    }
    warnings.extend(done.warnings.iter().cloned());
    // Where the files came from: the cache when the plan downloads nothing.
    let cached = done
        .plan
        .targets
        .iter()
        .flat_map(|t| &t.releases)
        .all(|r| r.url.is_none());
    let mut lines = Vec::new();
    for t in &mut data.targets {
        let mut made: Vec<SkippedNote> = done
            .outcomes
            .iter()
            .filter(|o| o.target == t.target.id)
            .flat_map(|o| &o.changes)
            .flat_map(|c| {
                c.policy
                    .skips_added
                    .iter()
                    .map(|m| SkippedNote::of(&c.addon, m))
            })
            .collect();
        made.sort();
        let changed = t.items.iter().any(|i| i.status.changed());
        if changed {
            lines.push(BackLine {
                target: t.target.id.clone(),
                addon: a.name.clone(),
                left: a.version.clone(),
                back: t
                    .items
                    .iter()
                    .find_map(|i| i.to_version.clone())
                    .unwrap_or_else(|| back.clone()),
                from: match source {
                    Source::Verified(_) | Source::Cache if cached => Source::Cache,
                    Source::Cache => Source::Verified(SourceId::of(&a.r#ref)),
                    other => other,
                },
                tx: t.tx,
                dry_run: ctx.dry_run,
            });
        }
        t.skips_marked = Some(made);
    }
    // A fix that names a label is never the way back from a hash mismatch: the
    // transaction's undo when it can run, else the releases listing.
    let undo_fix = along
        .filter(|&at| engine.undoable_now(&d.target.client_dir, entries, at))
        .map(|at| entries[at].tx);
    for t in &mut data.targets {
        for item in &mut t.items {
            if let Some(e) = item
                .error
                .as_mut()
                .filter(|e| e.code == ErrorCode::HashMismatch)
            {
                let fix = match undo_fix {
                    Some(tx) => Suggestion::command([
                        "omaddons".to_owned(),
                        "undo".to_owned(),
                        tx.to_string(),
                    ]),
                    None => releases_fix(&a.name, &id),
                };
                e.set_fix(fix);
                // The archive held to is the one installed before, not a lock's.
                if let Some(said) = e.message.strip_suffix("the lock records") {
                    let message = format!(
                        "{said}installed before: its source serves other files under {back} now"
                    );
                    e.set_message(message);
                }
            }
        }
    }
    data.recount();
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
    data.precondition = token;
    let data = data.listing(verbose);
    let mut commands = data.undo_commands();
    for t in &data.targets {
        if let Some(tx) = t.tx {
            commands.push(CommandHint::new(
                Action::Unskip,
                [
                    "omaddons".to_owned(),
                    "unskip".to_owned(),
                    "--tx".to_owned(),
                    tx.to_string(),
                    "-t".to_owned(),
                    t.target.id.clone(),
                ],
            ));
        }
    }
    let mut unique: Vec<Warning> = Vec::new();
    for w in warnings {
        if !unique.contains(&w) {
            unique.push(w);
        }
    }
    Ok(SkipOutcome {
        inner: TargetsOutcome {
            command,
            data,
            warnings: unique,
            commands,
            notes: Vec::new(),
            preview,
        },
        unskip: false,
        reasons: BTreeMap::new(),
        locks,
        notes,
        back: lines,
    })
}

/// The human output of a rollback run: each client's lines, then the shared tail.
pub(super) fn render_all(
    w: &mut dyn Write,
    outcome: &SkipOutcome,
    style: &Style,
) -> std::io::Result<()> {
    for line in &outcome.back {
        render(w, line)?;
    }
    let data = &outcome.inner.data;
    if outcome.back.is_empty() {
        writeln!(w, "Nothing to go back from.")?;
    }
    intent::render_plan_line(w, data.plan_id.as_ref(), data.plan_expires_at.as_ref())?;
    match data.precondition.as_deref() {
        Some(token) if style.detail() => intent::render_bind_line(w, token, data.plan_id.is_some()),
        _ => Ok(()),
    }
}
