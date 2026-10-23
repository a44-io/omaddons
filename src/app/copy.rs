//! `omaddons copy --from T1 --to T2 [<addon>…] [--strict] [--dry-run] [--no-deps]`: the
//! same addons on another client, where builds exist (G-M2-5).
//!
//! Each side is exactly one client (`resolve_one`; copy never fans out, M2-D24). Every
//! explicitly installed addon on T1, or each one named, is installed on T2 from its ref
//! in one transaction, unpinned (versions are flavor-specific) and on its recorded
//! channel. Dependency records are not copied: T2's own TOCs decide what it needs, and
//! the engine's dependency rounds install it (M2-D34). An addon already on T2 (by ref
//! identity) and a `file:` install are skipped and said so; an addon with no build that
//! loads on T2 is skipped too, never a failure (M2-D30). Any other failure makes the
//! run partial, which `--strict` turns into exit 6.

use std::collections::BTreeMap;

use super::engine::{Engine, recover_waiting};
use super::outcome::{
    ItemDeps, ItemError, ItemRequest, ItemResult, ItemStatus, SkipReason, TargetRef, TargetResult,
    TargetsData, TargetsOutcome, header, item_detail, item_name, sort_keeping_dependencies,
};
use super::resolve_args::catalog_name;
use super::world::{World, names_addon};
use crate::cli::Context;
use crate::discover::DiscoveredTarget;
use crate::discover::registry::resolve_one_in;
use crate::engine::plan::{PinIntent, RequestedChange};
use crate::engine::{TxOutcomes, TxRequest, TxTarget};
use crate::error::{CommandHint, Error, ErrorBody, ErrorCode, Result, Suggestion, Warning};
use crate::model::{AddonReason, AddonRecord, Ref, RunId, TxId, TxKind};
use crate::output::{Outcome, Style};
use crate::resolve::cache::Freshness;
use crate::resolve::catalog::Catalog;
use crate::rules::Family;
use crate::state::records::TargetRecord;

/// `copy`'s own arguments.
#[derive(Debug, Clone, Default, clap::Args)]
pub struct CopyArgs {
    /// The client to copy from (one client: id, key, game mode, family, product, flavor
    /// folder or path)
    #[arg(long, value_name = "TARGET")]
    pub from: String,
    /// The client to install on (one client)
    #[arg(long, value_name = "TARGET")]
    pub to: String,
    /// Only these addons of --from (names, or catalog names and aliases); default: every
    /// addon installed explicitly there
    #[arg(value_name = "ADDON")]
    pub addons: Vec<String>,
    /// Exit 6 when an addon fails to copy (skips never count)
    #[arg(long)]
    pub strict: bool,
    /// Install only the addons copied, not the ones their TOCs require on --to
    #[arg(long)]
    pub no_deps: bool,
}

/// `copy`'s outcome: the shared multi-target outcome with one target, `--to`.
#[derive(Debug)]
pub struct CopyOutcome {
    inner: TargetsOutcome,
    strict: bool,
    /// `--to`'s family, for the human skip line (`no build for tbc`).
    family: Family,
    /// Each committed transaction's global lock hold time, for `-v` log lines.
    pub locks: Vec<(TxId, u64)>,
    /// For `-v` log lines.
    pub notes: Vec<String>,
}

impl Outcome for CopyOutcome {
    type Data = TargetsData;

    fn command(&self) -> &'static str {
        "copy"
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
                format!("{failed} {noun} did not copy (--strict)"),
                Suggestion::text("see data.targets[].items[].error for each failed addon"),
            )
        })
    }

    /// `anniversary: installed 4 addons (tx 01M5…)`, the installs and failures, then one
    /// line of skips: `skipped: pfUI (no build for tbc), Questie (already installed)`.
    fn render_human(&self, w: &mut dyn std::io::Write, _style: &Style) -> std::io::Result<()> {
        let data = &self.inner.data;
        for t in &data.targets {
            writeln!(w, "{}", header("copy", data.dry_run, t))?;
            let shown: Vec<&ItemResult> = t
                .items
                .iter()
                .filter(|i| i.status != ItemStatus::Skipped)
                .collect();
            let width = shown
                .iter()
                .map(|i| item_name(i).chars().count())
                .max()
                .unwrap_or(0);
            for i in &shown {
                writeln!(
                    w,
                    "  {:<width$}  {}",
                    item_name(i),
                    item_detail(i, data.dry_run)
                )?;
                if let Some(error) = &i.error {
                    writeln!(w, "  {:<width$}  fix: {}", "", error.fix)?;
                }
            }
            // With nothing changed, the header already says why.
            let skips: Vec<String> = t
                .items
                .iter()
                .filter_map(|i| {
                    let why = match i.skip_reason? {
                        SkipReason::NoBuildForTarget => {
                            format!("no build for {}", self.family.as_str())
                        }
                        SkipReason::AlreadyInstalled => "already installed".to_owned(),
                        SkipReason::LocalSource => "local source".to_owned(),
                        other => other.phrase().to_owned(),
                    };
                    Some(format!("{} ({why})", item_name(i)))
                })
                .collect();
            if !skips.is_empty() && t.items.iter().any(|i| i.status.changed()) {
                writeln!(w, "  skipped: {}", skips.join(", "))?;
            }
        }
        if let Some(tx) = data.targets.iter().find_map(|t| t.tx) {
            writeln!(w, "undo: omaddons undo {tx}")?;
        }
        Ok(())
    }
}

/// One addon of `--from` in the run's scope.
struct InScope<'w> {
    record: &'w AddonRecord,
    /// What the user typed for it (its name when copying everything).
    request: String,
}

/// Runs `copy`.
pub fn run(ctx: &Context, args: &CopyArgs) -> Result<CopyOutcome> {
    if ctx.target.is_some() {
        // clap requires --from and --to, so the command without -t is the fix.
        return Err(Error::new(
            ErrorCode::Usage,
            "copy names its clients with --from and --to, not -t",
            crate::cli::without_target(&ctx.argv).map_or_else(
                || Suggestion::text("omaddons copy --from <client> --to <client>"),
                Suggestion::command,
            ),
        ));
    }
    let world = World::load(ctx)?;
    let verbose = ctx.output.style.verbose > 0;
    let mut warnings = world.warnings.clone();
    let mut notes = Vec::new();
    let (catalog, catalog_warnings) = world.catalog(ctx);
    warnings.extend(catalog_warnings);
    let from = side(ctx, &world, "--from", &args.from)?;
    let to = side(ctx, &world, "--to", &args.to)?;
    if from.target.id == to.target.id {
        return Err(Error::new(
            ErrorCode::Usage,
            format!(
                "--from and --to both name {}: copy needs two clients",
                to.target.id
            ),
            Suggestion::command(["omaddons", "targets"]),
        ));
    }
    let records = world.records()?;
    let source = records.get(&from.target.id);
    let destination = records.get(&to.target.id);
    let (scope, unknown) = in_scope(source, &catalog, &args.addons);

    let to_ref = TargetRef {
        id: to.target.id.clone(),
        key: to.target.key,
        mode: None,
    };
    let mut changes: Vec<RequestedChange> = Vec::new();
    let mut requested: Vec<ItemRequest> = Vec::new();
    let mut pushed: Vec<ItemResult> = Vec::new();
    for s in &scope {
        let a = s.record;
        // A catalog name with a build per family: the destination's own build, as
        // `install <name>` picks it; none where no variant covers its family.
        let variant = catalog
            .entries()
            .iter()
            .find(|e| e.variant.is_some() && e.r#ref == a.r#ref)
            .map(|e| e.name.clone());
        let wanted = match &variant {
            Some(name) => match catalog.resolve(name, Some(to.target.family)) {
                Some(entry) => entry.r#ref.clone(),
                None => {
                    pushed.push(skipped(s, SkipReason::NoBuildForTarget));
                    continue;
                }
            },
            None => a.r#ref.clone(),
        };
        let there = destination.and_then(|d| d.addons.values().find(|b| b.r#ref == wanted));
        if let Some(there) = there {
            pushed.push(ItemResult {
                from_version: Some(there.version.clone()),
                name: Some(there.name.clone()),
                ..skipped(s, SkipReason::AlreadyInstalled)
            });
            continue;
        }
        if matches!(a.r#ref, Ref::File { .. }) {
            pushed.push(skipped(s, SkipReason::LocalSource));
            continue;
        }
        changes.push(RequestedChange::Install {
            r#ref: wanted.clone(),
            channel: a.channel,
            channel_explicit: false,
            version: None,
            file: None,
            name: catalog_name(&catalog, &wanted),
            // Pins are not copied: versions are flavor-specific.
            pin: PinIntent::Keep,
            reason: AddonReason::Explicit,
            reinstall: false,
            blob_sha256: None,
            asset: None,
            revision: None,
        });
        requested.push(ItemRequest {
            request: s.request.clone(),
            target: to.target.id.clone(),
            addon: Some(a.name.clone()),
            r#ref: Some(wanted),
            version: None,
        });
    }
    for name in &unknown {
        let error = Error::new(
            ErrorCode::NotManaged,
            format!("{name} is not installed on {}", from.target.id),
            Suggestion::command(["omaddons", "list", "-t", from.target.id.as_str()]),
        );
        pushed.push(ItemResult {
            error: Some(ItemError::from(&error)),
            ..bare(name, ItemStatus::Failed)
        });
    }

    let run = RunId::new();
    let engine = Engine::new(ctx, &world, "copy")?;
    // Someone copying now expects now's releases, as `install` does.
    let freshness = if ctx.offline {
        Freshness::OfflineOnly
    } else {
        Freshness::Revalidate
    };
    let cache = engine.cache(freshness);
    let done: Option<TxOutcomes> = if changes.is_empty() {
        recover_waiting(ctx, &world, "copy");
        None
    } else {
        let req = TxRequest {
            targets: vec![TxTarget {
                discovered: to.clone(),
                changes,
                default_kind: TxKind::Install,
                dependency_locks: BTreeMap::new(),
            }],
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
            no_build_is_a_skip(&mut data, done);
            install_fixes(&mut data, &ctx.argv);
            for o in &done.outcomes {
                warnings.extend(o.warnings.iter().cloned());
                notes.extend(o.notes.iter().cloned());
            }
            warnings.extend(done.warnings.iter().cloned());
            data
        }
        None => TargetsData::assemble(&[], &[], &[], ctx.dry_run, run, &[]),
    };
    for item in pushed {
        data.push_item(to_ref.clone(), item);
    }
    if data.targets.is_empty() {
        data.push_target(TargetResult::empty(to_ref));
    }
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
                warning.message = format!("{}: {}: {}", t.target.id, item_name(i), e.message);
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
    let commands = data.undo_commands();
    let mut unique: Vec<Warning> = Vec::new();
    for w in warnings {
        if !unique.contains(&w) {
            unique.push(w);
        }
    }
    Ok(CopyOutcome {
        inner: TargetsOutcome {
            command: "copy",
            data,
            warnings: unique,
            commands,
            notes: Vec::new(),
        },
        strict: args.strict,
        family: to.target.family,
        locks,
        notes,
    })
}

/// The one client `selector` names for `flag`. An ambiguous one is `target_ambiguous`
/// with one fix per candidate, each naming it with `flag`.
fn side<'w>(
    ctx: &Context,
    world: &'w World,
    flag: &str,
    selector: &str,
) -> Result<&'w DiscoveredTarget> {
    resolve_one_in(selector, &world.resolved, &world.table, &ctx.argv).map_err(|error| {
        if !matches!(error.code, ErrorCode::TargetAmbiguous) {
            return error;
        }
        let candidates: Vec<String> = error.details["candidates"]
            .as_array()
            .map(|c| {
                c.iter()
                    .filter_map(|id| id.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        let argv = crate::cli::fix_argv(&ctx.argv);
        let Some((first, rest)) = candidates.split_first() else {
            return error;
        };
        let mut named = Error::new(
            ErrorCode::TargetAmbiguous,
            format!("{flag} {}", error.message),
            Suggestion::command(with_side(&argv, flag, first)),
        )
        .details(serde_json::json!({
            "selector": selector,
            "candidates": candidates,
            "flag": flag,
        }));
        for id in rest {
            named = named.alternative(Suggestion::command(with_side(&argv, flag, id)));
        }
        named
    })
}

/// `argv` with `flag`'s value replaced by `id`.
fn with_side(argv: &[String], flag: &str, id: &str) -> Vec<String> {
    let joined = format!("{flag}=");
    let mut out = Vec::with_capacity(argv.len());
    let mut args = argv.iter();
    while let Some(arg) = args.next() {
        if arg == flag {
            out.push(arg.clone());
            args.next();
            out.push(id.to_owned());
        } else if arg.starts_with(&joined) {
            out.push(format!("{joined}{id}"));
        } else {
            out.push(arg.clone());
        }
    }
    out
}

/// The addons of `--from` in scope, by name, and the names it has no record for. With no
/// names: every explicitly installed addon (dependencies follow by resolution on `--to`,
/// M2-D34). A name matches a record's name (any case), or a catalog name or alias whose
/// ref is the record's; naming a dependency copies it as explicit.
fn in_scope<'w>(
    source: Option<&'w TargetRecord>,
    catalog: &Catalog,
    names: &[String],
) -> (Vec<InScope<'w>>, Vec<String>) {
    let mut addons: Vec<&AddonRecord> = source
        .map(|r| r.addons.values().collect())
        .unwrap_or_default();
    addons.sort_by_key(|a| a.name.to_lowercase());
    if names.is_empty() {
        let scope = addons
            .into_iter()
            .filter(|a| a.reason == AddonReason::Explicit)
            .map(|a| InScope {
                record: a,
                request: a.name.clone(),
            })
            .collect();
        return (scope, Vec::new());
    }
    let mut scope: Vec<InScope<'w>> = Vec::new();
    let mut unknown = Vec::new();
    for name in names {
        let mut found = false;
        for a in addons.iter().filter(|a| names_addon(name, a, catalog)) {
            found = true;
            if !scope.iter().any(|s| s.record.name == a.name) {
                scope.push(InScope {
                    record: a,
                    request: name.clone(),
                });
            }
        }
        if !found {
            unknown.push(name.clone());
        }
    }
    (scope, unknown)
}

fn bare(request: &str, status: ItemStatus) -> ItemResult {
    ItemResult {
        request: request.to_owned(),
        name: None,
        r#ref: None,
        status,
        skip_reason: None,
        duplicate_of: None,
        reason: None,
        from_version: None,
        to_version: None,
        folders: None,
        required_by: None,
        deps: ItemDeps::default(),
        error: None,
    }
}

fn skipped(s: &InScope<'_>, reason: SkipReason) -> ItemResult {
    ItemResult {
        name: Some(s.record.name.clone()),
        r#ref: Some(s.record.r#ref.clone()),
        skip_reason: Some(reason),
        ..bare(&s.request, ItemStatus::Skipped)
    }
}

/// The engine words its retries as this command with more arguments (`--force`, `-t
/// <sibling>`), which `copy` takes none of: each becomes the install of that item with
/// them, on `--to` unless they name another client.
fn install_fixes(data: &mut TargetsData, argv: &[String]) {
    let argv = crate::cli::fix_argv(argv);
    for t in &mut data.targets {
        let id = t.target.id.clone();
        for i in &mut t.items {
            let Some(error) = &mut i.error else {
                continue;
            };
            let Some(extra) = error
                .fix_argv
                .as_ref()
                .and_then(|fix| fix.strip_prefix(argv.as_slice()))
                .filter(|extra| !extra.is_empty())
            else {
                continue;
            };
            let what = i.r#ref.as_ref().map_or_else(
                || i.name.clone().unwrap_or_else(|| i.request.clone()),
                ToString::to_string,
            );
            let mut install = vec!["omaddons".to_owned(), "install".to_owned(), what];
            install.extend(extra.iter().cloned());
            if !extra.iter().any(|a| a == "-t" || a == "--target") {
                install.extend(["-t".to_owned(), id.clone()]);
            }
            let fix = Suggestion::command(install);
            error.fix = fix.display().to_owned();
            error.fix_argv = fix.argv().map(<[String]>::to_vec);
        }
    }
}

/// A copied addon with no build that loads on `--to` is skipped, not failed (M2-D30). A
/// dependency's refusal stays a failure: the requester needs it (M2-D9).
fn no_build_is_a_skip(data: &mut TargetsData, done: &TxOutcomes) {
    for t in &mut data.targets {
        let id = t.target.id.clone();
        for i in t
            .items
            .iter_mut()
            .filter(|i| i.status == ItemStatus::Failed)
        {
            let failure = done.failures.iter().find(|f| {
                f.target == id
                    && (f.r#ref.is_some() && f.r#ref == i.r#ref
                        || f.addon.as_deref().is_some_and(|a| {
                            i.name.as_deref().is_some_and(|n| n.eq_ignore_ascii_case(a))
                        }))
            });
            if failure
                .is_some_and(|f| no_build(&f.error) && f.error.details.get("required_by").is_none())
            {
                i.status = ItemStatus::Skipped;
                i.skip_reason = Some(SkipReason::NoBuildForTarget);
                i.error = None;
            }
        }
    }
    data.recount();
}

/// The release exists, but nothing in it loads on this client (`install`'s
/// client-specific errors).
fn no_build(error: &ErrorBody) -> bool {
    matches!(
        error.code,
        ErrorCode::NoCompatibleRelease | ErrorCode::WrongFlavor
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(s: &str) -> Vec<String> {
        s.split(' ').map(str::to_owned).collect()
    }

    #[test]
    fn a_side_is_renamed_where_it_was_given() {
        assert_eq!(
            with_side(
                &argv("omaddons copy --from vanilla --to anniversary"),
                "--from",
                "era"
            ),
            argv("omaddons copy --from era --to anniversary")
        );
        assert_eq!(
            with_side(
                &argv("omaddons copy --from=vanilla --to tbc Foo"),
                "--from",
                "era"
            ),
            argv("omaddons copy --from=era --to tbc Foo")
        );
        assert_eq!(
            with_side(
                &argv("omaddons copy --to tbc --from era"),
                "--to",
                "anniversary"
            ),
            argv("omaddons copy --to anniversary --from era")
        );
    }

    #[test]
    fn only_client_refusals_are_missing_builds() {
        let body = |code| ErrorBody::from(Error::new(code, "x", Suggestion::text("y")));
        assert!(no_build(&body(ErrorCode::NoCompatibleRelease)));
        assert!(no_build(&body(ErrorCode::WrongFlavor)));
        assert!(!no_build(&body(ErrorCode::Network)));
        assert!(!no_build(&body(ErrorCode::NotFound)));
    }
}
