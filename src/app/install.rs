//! `omaddons install <ref|url|path>... [-t T] [--channel C] [--version V] [--file ZIP]
//! [--no-deps] [--force] [--strict]`: puts addons from keyless sources into one target,
//! verified against that client's own TOC rules, through the journaled engine, and
//! undoable. Installing an installed addon whose source has a newer release updates it.
//!
//! The target: `-t`, else the configured default, else the single live target every
//! requested addon verifies on (test clients only ever with `-t`). Several compatible
//! targets are `target_ambiguous` with one exact command per candidate. Items fail
//! independently: some failing is `partial` (exit 0, or 6 with `--strict`); all failing
//! is the first item's error.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::PathBuf;

use schemars::JsonSchema;
use serde::Serialize;
use serde_json::json;

use super::engine::Engine;
use super::outcome::{
    DepItem, ItemDeps, addon_identity, dependency_items, dependency_suffix, sorted,
};
pub use super::outcome::{ItemError, ItemStatus, ModeNote, SkipReason, TargetRef};
use super::resolve_args::{aimed_at, catalog_name, for_family, parse_ref};
use super::world::World;
use crate::cli::Context;
use crate::discover::DiscoveredTarget;
use crate::discover::registry::{resolve_one, with_target};
use crate::engine::edit::EditField;
use crate::engine::plan::{
    ChangeKind, FolderSummary, PinIntent, PlanFailure, RequestedChange, TargetPlan, Unresolved,
    UnresolvedReason,
};
use crate::engine::{TxOutcome, TxOutcomes, TxRequest, TxTarget};
use crate::error::{CommandHint, Error, ErrorCode, Result, Suggestion, Warning, WarningCode};
use crate::model::{
    AddonReason, AddonRecord, Channel, FolderName, Ref, ReloadRequired, RunId, TxId, TxKind,
    VersionConfidence, WowAceRev,
};
use crate::output::{Outcome, Style};
use crate::resolve::cache::{Freshness, ReleaseCache};
use crate::resolve::deps::BridgeDependency;
use crate::rules::{RulesTable, TargetKind};
use crate::sources::refs::ParsedRef;
use crate::state::memo::CandidateFact;
use crate::state::records::{TargetRecord, load_record};

/// `install`'s own flags.
#[derive(Debug, Clone, Default, clap::Args)]
pub struct InstallArgs {
    /// What to install: a catalog name (weakauras), owner/repo, a canonical ref (gh:, git:,
    /// url:, file:, wowi:, tukui:), a pasted GitHub or https link, or a path to a zip or
    /// folder
    #[arg(required = true, value_name = "REF")]
    pub refs: Vec<String>,
    /// stable, beta or alpha (default: the addon's recorded channel, then
    /// default_channel, then stable)
    #[arg(long, value_name = "CHANNEL", value_parser = ["stable", "beta", "alpha"])]
    pub channel: Option<String>,
    /// Install exactly this release: a tag for gh: refs, a rev for git: refs
    #[arg(long, value_name = "VERSION")]
    pub version: Option<String>,
    /// Install this downloaded archive as the release of the one given ref
    #[arg(long, value_name = "ZIP")]
    pub file: Option<PathBuf>,
    /// Install the recorded release again, replacing its folders (repairs modified or
    /// missing files)
    #[arg(long)]
    pub reinstall: bool,
    /// Install only the named addons, not the ones their TOCs require
    #[arg(long)]
    pub no_deps: bool,
    /// Take over a folder another addon owns, an addon of the same name from another ref,
    /// or replace an unmanaged folder
    #[arg(long)]
    pub force: bool,
    /// Exit 6 when only some of the addons install
    #[arg(long)]
    pub strict: bool,
    /// Fetch every release from its source, never from remembered metadata
    #[arg(long)]
    pub refresh: bool,
}

/// `install`'s data.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct InstallData {
    pub dry_run: bool,
    pub target: TargetRef,
    pub tx: Option<TxId>,
    /// `null` when nothing changed.
    pub kind: Option<TxKind>,
    /// In argument order.
    pub items: Vec<InstallItem>,
    pub reload_required: ReloadRequired,
    /// `null` when no swap happened.
    pub atomic_swap: Option<bool>,
    /// The client has no `Interface/AddOns` yet (it creates one on its first launch), so
    /// this install creates it (under `--dry-run`: would create it).
    pub creates_addons_dir: bool,
    /// `-v` only: the target's client dir.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_dir: Option<PathBuf>,
}

/// One requested addon.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct InstallItem {
    /// The argument exactly as given.
    pub request: String,
    #[serde(rename = "ref")]
    pub r#ref: Option<Ref>,
    pub name: Option<String>,
    pub status: ItemStatus,
    /// `skipped` items only: why (`duplicate`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skip_reason: Option<SkipReason>,
    /// A duplicate's first request, which it collapsed into.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duplicate_of: Option<String>,
    /// The record fields an `edited` item changed, sorted; omitted when empty.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<EditField>,
    pub from_version: Option<String>,
    pub to_version: Option<String>,
    pub version_confidence: Option<VersionConfidence>,
    pub channel: Option<Channel>,
    pub folders: FolderSummary,
    pub error: Option<ItemError>,
    /// `explicit` or `dependency`; `null` when no record is known (a parse failure).
    pub reason: Option<AddonReason>,
    /// The addons that require it.
    pub required_by: Vec<String>,
    /// What its other folders (bridges, plugins) work with that is not installed.
    pub bridge_dependencies: Vec<BridgeDependency>,
    /// The required folders nothing was installed for, and why.
    pub unresolved: Vec<Unresolved>,
    /// `-v` only: where the installed release came from.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub asset_url: Option<String>,
    /// `-v` only: its needed folders' `OptionalDeps`, never installed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub optional_dependencies: Option<Vec<FolderName>>,
}

impl InstallItem {
    /// An item with nothing but its request and status.
    fn bare(request: String, r#ref: Option<Ref>, status: ItemStatus) -> Self {
        Self {
            request,
            r#ref,
            name: None,
            status,
            skip_reason: None,
            duplicate_of: None,
            fields: Vec::new(),
            from_version: None,
            to_version: None,
            version_confidence: None,
            channel: None,
            folders: FolderSummary::default(),
            error: None,
            reason: None,
            required_by: Vec::new(),
            bridge_dependencies: Vec::new(),
            unresolved: Vec::new(),
            asset_url: None,
            optional_dependencies: None,
        }
    }

    /// Its record's `reason` and `required_by`, and what `tp` reports about its
    /// dependencies (`optional_dependencies` under `verbose`).
    fn with_dependencies(
        mut self,
        record: Option<&AddonRecord>,
        tp: Option<&TargetPlan>,
        verbose: bool,
    ) -> Self {
        if let Some(record) = record {
            self.reason = Some(record.reason);
            self.required_by.clone_from(&record.required_by);
        }
        let name = self.name.clone().unwrap_or_else(|| self.request.clone());
        if let Some(tp) = tp {
            let deps = ItemDeps::of(tp, &name);
            self.bridge_dependencies = deps.bridge_dependencies.unwrap_or_default();
            self.unresolved = deps.unresolved.unwrap_or_default();
            self.optional_dependencies =
                verbose.then_some(deps.optional_dependencies.unwrap_or_default());
        }
        self
    }
}

/// `install`'s outcome.
#[derive(Debug)]
pub struct InstallOutcome {
    data: InstallData,
    warnings: Vec<Warning>,
    commands: Vec<CommandHint>,
    strict: bool,
    /// `--reinstall`: an updated item at the same version was reinstalled.
    reinstall: bool,
    /// The requests reinstalled from the blob cache, for the human line.
    from_cache: Vec<String>,
    /// The global lock's hold time, for the `-v` log line.
    pub lock_held_ms: Option<u64>,
    /// Why addons were left unchanged, for `-v` log lines.
    pub notes: Vec<String>,
}

impl InstallOutcome {
    /// Failed items, dependencies included.
    fn failed(&self) -> usize {
        self.data
            .items
            .iter()
            .filter(|i| i.status == ItemStatus::Failed)
            .count()
    }
}

impl Outcome for InstallOutcome {
    type Data = InstallData;

    fn command(&self) -> &'static str {
        "install"
    }

    fn data(&self) -> &InstallData {
        &self.data
    }

    fn warnings(&self) -> &[Warning] {
        &self.warnings
    }

    fn commands(&self) -> &[CommandHint] {
        &self.commands
    }

    fn partial(&self) -> bool {
        self.failed() > 0
    }

    fn assertion_failure(&self) -> Option<Error> {
        (self.strict && self.partial()).then(|| {
            Error::new(
                ErrorCode::CheckFailed,
                format!(
                    "{} of {} addons did not install (--strict)",
                    self.failed(),
                    self.data.items.len()
                ),
                Suggestion::text("see data.items[].error for each failed addon"),
            )
        })
    }

    fn render_human(&self, w: &mut dyn Write, _style: &Style) -> std::io::Result<()> {
        let d = &self.data;
        let id = &d.target.id;
        if let Some(mode) = d.target.mode.as_ref().filter(|m| !m.shared_with.is_empty()) {
            writeln!(
                w,
                "{id}: {} runs in this client and shares its addons with {}",
                mode.name,
                and_list(&mode.shared_with)
            )?;
        }
        if d.creates_addons_dir {
            let verb = if d.dry_run { "would create" } else { "created" };
            writeln!(
                w,
                "{id}: {verb} Interface/AddOns (this client has not been launched yet)"
            )?;
        }
        for item in &d.items {
            let name = item.name.as_deref().unwrap_or(&item.request);
            let folders = item.folders.added.len() + item.folders.replaced.len();
            let dependency = dependency_suffix(item.reason, &item.required_by);
            match item.status {
                ItemStatus::Installed | ItemStatus::Updated => {
                    let reinstalled = self.reinstall
                        && item.status == ItemStatus::Updated
                        && item.from_version == item.to_version;
                    let verb = match (item.status, d.dry_run) {
                        (_, false) if reinstalled => "reinstalled".to_owned(),
                        (_, true) if reinstalled => "would reinstall".to_owned(),
                        (status, dry_run) => status.verb(dry_run),
                    };
                    let from = if reinstalled && self.from_cache.contains(&item.request) {
                        " from the local cache"
                    } else {
                        ""
                    };
                    writeln!(
                        w,
                        "{id}: {verb} {name} {}{from} ({}){dependency}",
                        item.to_version.as_deref().unwrap_or("?"),
                        crate::output::count(folders, "folder")
                    )?;
                    let skipped: Vec<&str> = item
                        .folders
                        .skipped
                        .iter()
                        .map(|s| s.name.as_str())
                        .collect();
                    if !skipped.is_empty() {
                        let verb = if d.dry_run { "would skip" } else { "skipped" };
                        let noun = if skipped.len() == 1 {
                            "folder"
                        } else {
                            "folders"
                        };
                        writeln!(
                            w,
                            "  {verb} {} {noun} this client does not load: {}",
                            skipped.len(),
                            skipped.join(", ")
                        )?;
                    }
                }
                ItemStatus::Unchanged => {
                    writeln!(
                        w,
                        "{id}: {name} unchanged ({})",
                        item.to_version.as_deref().unwrap_or("?")
                    )?;
                }
                ItemStatus::Skipped => {
                    let first = item.duplicate_of.as_deref().unwrap_or("?");
                    writeln!(
                        w,
                        "{id}: skipped {}: the same addon as {first}",
                        item.request
                    )?;
                }
                // Install reports adoptions and removals as installs.
                ItemStatus::Pinned
                | ItemStatus::Unpinned
                | ItemStatus::Edited
                | ItemStatus::Removed
                | ItemStatus::Adopted => {
                    let verb = item.status.verb(d.dry_run);
                    let version = item.to_version.as_deref().unwrap_or("?");
                    if item.fields.is_empty() {
                        writeln!(w, "{id}: {verb} {name} at {version}")?;
                    } else {
                        let fields: Vec<&str> = item.fields.iter().map(|f| f.wire()).collect();
                        writeln!(
                            w,
                            "{id}: {verb} {name} at {version} ({})",
                            fields.join(", ")
                        )?;
                    }
                }
                ItemStatus::Failed => {
                    let (code, message) = item
                        .error
                        .as_ref()
                        .map_or(("", ""), |e| (e.code.wire(), e.message.as_str()));
                    writeln!(w, "{id}: failed {name}{dependency}: {code}: {message}")?;
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

/// One positional, parsed.
struct Item {
    request: String,
    parsed: std::result::Result<ParsedRef, Error>,
}

/// Runs `install`.
pub fn run(ctx: &Context, args: &InstallArgs) -> Result<InstallOutcome> {
    if args.file.is_some() && args.refs.len() != 1 {
        return Err(usage(
            "--file installs one archive as the release of exactly one ref",
        ));
    }
    let cwd = std::env::current_dir().unwrap_or_default();
    let file = args.file.as_ref().map(|f| cwd.join(f));
    let world = World::load(ctx)?;
    let mut warnings = world.warnings.clone();
    let (catalog, catalog_warnings) = world.catalog(ctx);
    warnings.extend(catalog_warnings);
    let records = world.records()?;

    // Parse every positional; parse failures are failed items.
    let items: Vec<Item> = args
        .refs
        .iter()
        .map(|request| Item {
            request: request.clone(),
            parsed: parse_ref(&world.config.git_hosts, &records, &catalog, &cwd, request)
                .map_err(|e| aimed_at(e, &ctx.argv, request)),
        })
        .collect();
    // clap admits only these three.
    let channel = args.channel.as_deref().map(|c| match c {
        "beta" => Channel::Beta,
        "alpha" => Channel::Alpha,
        _ => Channel::Stable,
    });
    for item in &items {
        if let Ok(p) = &item.parsed {
            if args.version.is_some() && matches!(p.r#ref, Ref::Url { .. } | Ref::File { .. }) {
                return Err(usage(&format!(
                    "--version applies to gh: and git: refs, not {}",
                    p.r#ref
                )));
            }
            if let (Some(v), Ref::Git { rev: Some(rev), .. }) = (&args.version, &p.r#ref)
                && v != rev
            {
                return Err(usage(&format!(
                    "--version {v} disagrees with the rev {rev} that {} names",
                    item.request
                )));
            }
            if let (Some(v), Ref::WowAce { rev, .. }) = (&args.version, &p.r#ref) {
                let without = crate::cli::without_flag(&ctx.argv, "--version");
                let fix = Suggestion::command(crate::cli::fix_argv(&without));
                if WowAceRev::parse(v).is_none() {
                    return Err(Error::new(
                        ErrorCode::Usage,
                        format!(
                            "--version on a wowace: ref takes a tag (v12.1.0), a trunk revision (r152) or a directory at a revision (r43:<path>), not {v:?}"
                        ),
                        Suggestion::command(["omaddons", "help", "install"]),
                    ));
                }
                if let Some(rev) = rev.as_ref().filter(|rev| rev.to_string() != *v) {
                    return Err(Error::new(
                        ErrorCode::Usage,
                        format!(
                            "--version {v} disagrees with the {rev} that {} names; give one of the two",
                            item.request
                        ),
                        fix,
                    ));
                }
            }
            if let (Some(v), Some(hint)) = (&args.version, &p.version_hint)
                && v.trim_start_matches('v') != hint.trim_start_matches('v')
            {
                return Err(usage(&format!(
                    "--version {v} disagrees with the release {hint} that {} names",
                    item.request
                )));
            }
        }
    }

    let engine = Engine::new(ctx, &world, "install")?;
    let change_for = |p: &ParsedRef, target: &DiscoveredTarget| {
        let version = args.version.clone().or_else(|| p.version_hint.clone());
        let installed = recorded(&records, target, &p.r#ref);
        RequestedChange::Install {
            r#ref: p.r#ref.clone(),
            channel: channel
                .or_else(|| installed.map(|a| a.channel))
                .or(world.config.default_channel)
                .unwrap_or_default(),
            channel_explicit: channel.is_some(),
            // A version (or a release link's tag) pins; otherwise a pin stays (M2-D29).
            pin: if version.is_some() {
                PinIntent::Pin
            } else {
                PinIntent::Keep
            },
            version,
            file: file.clone(),
            name: catalog_name(&catalog, &p.r#ref),
            // Asking for an addon by name makes it explicit, promoting a dependency; a
            // reinstall is a repair and keeps the recorded reason.
            reason: match installed {
                Some(a) if args.reinstall => a.reason,
                _ => AddonReason::Explicit,
            },
            reinstall: args.reinstall,
            blob_sha256: None,
            // A pasted release-asset link names its one asset.
            asset: p.asset_hint.clone(),
            revision: None,
        }
    };
    let parsed: Vec<&ParsedRef> = items
        .iter()
        .filter_map(|i| i.parsed.as_ref().ok())
        .collect();

    if parsed.is_empty() {
        let errors: Vec<(String, Error)> = items
            .iter()
            .filter_map(|i| {
                i.parsed
                    .as_ref()
                    .err()
                    .map(|e| (i.request.clone(), e.clone()))
            })
            .collect();
        return Err(all_failed(&errors));
    }
    // Someone installing now expects now's release; --offline answers from the memo.
    let freshness = if ctx.offline {
        Freshness::OfflineOnly
    } else if args.refresh {
        Freshness::Refresh
    } else {
        Freshness::Revalidate
    };
    let cache = engine.cache(freshness);
    // What both engine runs learned by downloading candidates.
    let mut facts = Vec::new();
    // A catalog name with variants names each client's own (M3-D16).
    let resolve_for = |p: &ParsedRef, request: &str, d: &DiscoveredTarget| {
        for_family(p, &catalog, d.target.family, request)
    };
    let chosen = choose_target(
        ctx,
        &world,
        &engine,
        &cache,
        &mut facts,
        &items,
        (&change_for, &resolve_for),
    );
    let target = match chosen {
        Ok(target) => target,
        Err(error) => {
            // What the probe fetched is still worth remembering.
            cache.remember(&facts);
            return Err(error);
        }
    };
    let items: Vec<Item> = items
        .into_iter()
        .map(|i| Item {
            parsed: i.parsed.and_then(|p| resolve_for(&p, &i.request, target)),
            request: i.request,
        })
        .collect();
    let parsed: Vec<&ParsedRef> = items
        .iter()
        .filter_map(|i| i.parsed.as_ref().ok())
        .collect();
    if parsed.is_empty() {
        cache.remember(&facts);
        let errors: Vec<(String, Error)> = items
            .iter()
            .filter_map(|i| {
                i.parsed
                    .as_ref()
                    .err()
                    .map(|e| (i.request.clone(), e.clone()))
            })
            .collect();
        return Err(all_failed(&errors));
    }

    let req = TxRequest {
        targets: vec![TxTarget {
            discovered: target.clone(),
            changes: parsed.iter().map(|p| change_for(p, target)).collect(),
            default_kind: TxKind::Install,
            dependency_locks: BTreeMap::new(),
        }],
        force: args.force,
        while_running: ctx.while_running,
        dry_run: ctx.dry_run,
        reason: ctx.reason.clone(),
        kind: None,
        run: RunId::new(),
        no_deps: args.no_deps,
    };
    let done = engine.run(&req, Some(&cache));
    // The command's last engine run: remember what it fetched, even when it failed.
    if let Ok(done) = &done {
        facts.extend(done.plan.facts.iter().cloned());
    }
    let remembered = cache.remember(&facts);
    let done = done?;
    warnings.extend(remembered.warnings);
    let record = target
        .target
        .key
        .map(|k| load_record(&world.paths, &k))
        .transpose()?
        .map(|l| l.value);
    let outcome = done.outcomes.first();
    let after = outcome
        .and_then(|o| o.key)
        .map(|k| load_record(&world.paths, &k))
        .transpose()?
        .map(|l| l.value)
        .or(record);
    let verbose = ctx.output.style.verbose > 0;
    let mut built = build_items(&items, &done, outcome, after.as_ref(), verbose);
    // Only what is installed can be reinstalled: the fix installs it.
    if args.reinstall {
        let id = outcome.map_or_else(|| target.target.id.clone(), |o| o.target.clone());
        for (item, error) in &mut built {
            if let Some(e) = error.as_mut().filter(|e| e.code == ErrorCode::NotManaged) {
                let fix = Suggestion::command(["omaddons", "install", &item.request, "-t", &id]);
                *e = Error::new(e.code, e.message.clone(), fix);
                item.error = Some(ItemError::from(&*e));
            }
        }
    }
    // Reinstalls from a release with no download URL: the blob cache.
    let from_cache: Vec<String> = built
        .iter()
        .filter(|(i, _)| args.reinstall && i.status == ItemStatus::Updated)
        .filter(|(i, _)| {
            let planned = done
                .plan
                .targets
                .iter()
                .flat_map(|t| &t.work)
                .find_map(|w| w.record().filter(|r| Some(&r.r#ref) == i.r#ref.as_ref()));
            let release = planned.and_then(|r| r.release_id.as_ref()).and_then(|id| {
                done.plan
                    .targets
                    .iter()
                    .flat_map(|t| &t.releases)
                    .find(|r| &r.release_id == id)
            });
            release.is_some_and(|r| r.url.is_none())
        })
        .map(|(i, _)| i.request.clone())
        .collect();
    let errors: Vec<(String, Error)> = built
        .iter()
        .filter_map(|(item, e)| e.clone().map(|e| (item.request.clone(), e)))
        .collect();
    if errors.len() == built.len() {
        return Err(all_failed(&errors));
    }
    for (request, error) in &errors {
        let mut warning = Warning::item(error);
        if !error.message.contains(request.as_str()) {
            warning.message = format!("{request}: {}", error.message);
        }
        warnings.push(warning);
    }
    let built: Vec<InstallItem> = built.into_iter().map(|(item, _)| item).collect();
    let built = with_dependency_items(built, &done, outcome, verbose);
    // A dependency that resolved but did not install: a failed item (the run is partial,
    // M2-D9), and a warning saying what required it.
    let failed_dependencies: Vec<&PlanFailure> = done
        .failures
        .iter()
        .filter(|f| f.error.details.get("required_by").is_some())
        .collect();
    for f in &failed_dependencies {
        let Some(cause) = &f.cause else {
            continue;
        };
        let by: Vec<&str> = f.error.details["required_by"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(serde_json::Value::as_str)
            .collect();
        let dependency = f
            .r#ref
            .as_ref()
            .map_or_else(|| "a dependency".to_owned(), ToString::to_string);
        let mut warning = Warning::item(cause);
        warning.message = format!(
            "{dependency}, required by {}, did not install: {}",
            by.join(", "),
            cause.message
        );
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
    let mut commands: Vec<CommandHint> = tx
        .map(|tx| {
            vec![CommandHint::new(
                "undo",
                ["omaddons".to_owned(), "undo".to_owned(), tx.to_string()],
                false,
            )]
        })
        .unwrap_or_default();
    // Required folders present but unmanaged: nothing installed for them; one adopt
    // brings them all under management as they are.
    let mut unmanaged: Vec<&FolderName> = done
        .plan
        .targets
        .iter()
        .flat_map(|t| t.dependencies.values())
        .flat_map(|r| &r.unmanaged)
        .collect();
    unmanaged.sort_by(|a, b| a.key().cmp(b.key()));
    unmanaged.dedup();
    if !unmanaged.is_empty() {
        let id = outcome.map_or_else(|| target.target.id.clone(), |o| o.target.clone());
        let argv = ["omaddons", "adopt"]
            .into_iter()
            .chain(unmanaged.iter().map(|f| f.as_str()))
            .chain(["-t", id.as_str()]);
        commands.push(CommandHint::new("adopt", argv, false));
    }
    let id = outcome.map_or_else(|| target.target.id.clone(), |o| o.target.clone());
    commands.extend(install_hints(&built, &unique, &id));
    let swapped = tx.is_some();
    Ok(InstallOutcome {
        data: InstallData {
            dry_run: ctx.dry_run,
            target: TargetRef {
                id: outcome.map_or_else(|| target.target.id.clone(), |o| o.target.clone()),
                key: outcome.and_then(|o| o.key).or(target.target.key),
                mode: ctx
                    .target
                    .as_deref()
                    .and_then(|s| mode_note(s, target, &world.table)),
            },
            tx,
            kind: outcome.and_then(|o| o.kind),
            items: built,
            reload_required: outcome.map_or(ReloadRequired::None, |o| o.reload_required),
            atomic_swap: swapped.then(|| outcome.is_none_or(|o| o.atomic_swap)),
            creates_addons_dir: outcome.is_some_and(|o| o.creates_addons_dir),
            client_dir: verbose.then(|| target.target.client_dir.clone()),
        },
        warnings: unique,
        commands,
        strict: args.strict,
        reinstall: args.reinstall,
        from_cache,
        lock_held_ms: outcome.and_then(|o| o.lock_held_ms),
        notes: outcome
            .map(|o| o.notes.clone())
            .unwrap_or_default()
            .into_iter()
            .chain(remembered.notes)
            .collect(),
    })
}

/// The record of `r` on `target`, when it is installed there.
fn recorded<'r>(
    records: &'r BTreeMap<String, TargetRecord>,
    target: &DiscoveredTarget,
    r: &Ref,
) -> Option<&'r AddonRecord> {
    records
        .get(&target.target.id)?
        .addons
        .values()
        .find(|a| a.r#ref == *r)
}

/// The change an install asks of a target for one parsed argument.
type ChangeFor<'a> = &'a dyn Fn(&ParsedRef, &DiscoveredTarget) -> RequestedChange;
/// What a parsed argument (with its request) names on a target: a catalog name with
/// variants, that target's own.
type ResolveFor<'a> = &'a dyn Fn(&ParsedRef, &str, &DiscoveredTarget) -> Result<ParsedRef>;

/// `-t`, else the configured default, else the single live target every addon
/// verifies on. Test clients (PTR, beta) are only ever chosen with `-t`.
fn choose_target<'w>(
    ctx: &Context,
    world: &'w World,
    engine: &Engine,
    cache: &ReleaseCache<'_>,
    facts: &mut Vec<CandidateFact>,
    items: &[Item],
    (change_for, resolve_for): (ChangeFor<'_>, ResolveFor<'_>),
) -> Result<&'w DiscoveredTarget> {
    let targets = &world.resolved.targets;
    if let Some(selector) = &ctx.target {
        return resolve_one(selector, targets, &world.table, &ctx.argv);
    }
    if let Some(found) = world.default_target(&ctx.argv) {
        return found;
    }
    let mut live: Vec<&DiscoveredTarget> = targets
        .iter()
        .filter(|d| d.target.kind == TargetKind::Live)
        .collect();
    live.sort_by(|a, b| a.target.id.cmp(&b.target.id));
    match live.as_slice() {
        [] => return Err(no_live_target(ctx, targets)),
        [only] => return Ok(only),
        _ => {}
    }

    // Probe: plan every addon on every live target, without changing anything
    // (downloads land in the blob cache the real run then reuses). A catalog name with
    // variants is each target's own, and none where no variant covers its family.
    let parsed: Vec<(&str, &ParsedRef)> = items
        .iter()
        .filter_map(|i| Some((i.request.as_str(), i.parsed.as_ref().ok()?)))
        .collect();
    let on = |(request, p): (&str, &ParsedRef), d: &DiscoveredTarget| resolve_for(p, request, d);
    let probe = TxRequest {
        targets: live
            .iter()
            .map(|d| TxTarget {
                discovered: (*d).clone(),
                changes: parsed
                    .iter()
                    .filter_map(|p| on(*p, d).ok())
                    .map(|p| change_for(&p, d))
                    .collect(),
                default_kind: TxKind::Install,
                dependency_locks: BTreeMap::new(),
            })
            .collect(),
        force: false,
        while_running: true,
        dry_run: true,
        reason: None,
        kind: None,
        // A dry run commits nothing, so its run id is never recorded.
        run: RunId::new(),
        // The requested refs alone choose the target.
        no_deps: true,
    };
    // The same cache as the real run: each ref is fetched once across both.
    let probed = engine.run(&probe, Some(cache))?;
    facts.extend(probed.plan.facts.iter().cloned());
    // Why an addon fails on a target, and whether that is the target's alone: a catalog
    // name with no build for its family is, like a release that does not load there.
    let failure = |d: &DiscoveredTarget, p: (&str, &ParsedRef)| -> Option<(Error, bool)> {
        let resolved = match on(p, d) {
            Ok(resolved) => resolved,
            Err(error) => return Some((error, true)),
        };
        probed
            .failures
            .iter()
            .find(|f| f.target == d.target.id && f.r#ref.as_ref() == Some(&resolved.r#ref))
            .and_then(|f| f.cause.clone())
            .map(|e| {
                let specific = client_specific(e.code);
                (e, specific)
            })
    };
    // An addon that fails on every client cannot choose one.
    let fails_everywhere = |p: (&str, &ParsedRef)| live.iter().all(|d| failure(d, p).is_some());
    if parsed.iter().all(|p| fails_everywhere(*p)) {
        let errors: Vec<(String, Error)> = items
            .iter()
            .map(|i| {
                let error = match &i.parsed {
                    Err(e) => e.clone(),
                    Ok(p) => live
                        .iter()
                        .filter_map(|d| failure(d, (&i.request, p)))
                        .find(|(_, specific)| !specific)
                        .map(|(e, _)| e)
                        .unwrap_or_else(|| no_compatible(&i.request, &live)),
                };
                (i.request.clone(), error)
            })
            .collect();
        return Err(all_failed(&errors));
    }
    let deciding: Vec<(&str, &ParsedRef)> = parsed
        .iter()
        .copied()
        .filter(|p| !fails_everywhere(*p))
        .collect();
    let compatible: Vec<&DiscoveredTarget> = live
        .iter()
        .copied()
        .filter(|d| {
            deciding
                .iter()
                .all(|p| failure(d, *p).is_none_or(|(_, specific)| !specific))
        })
        .collect();
    let requests = deciding
        .iter()
        .map(|(request, _)| *request)
        .collect::<Vec<_>>()
        .join(", ");
    match compatible.as_slice() {
        [] => Err(no_compatible(&requests, &live)),
        [one] => Ok(one),
        many => {
            let ids: Vec<&str> = many.iter().map(|d| d.target.id.as_str()).collect();
            let mut error = Error::new(
                ErrorCode::TargetAmbiguous,
                format!(
                    "{requests} is compatible with {} targets: {}",
                    ids.len(),
                    ids.join(", ")
                ),
                Suggestion::command(with_target(&ctx.argv, ids[0])),
            )
            .details(json!({ "candidates": ids }));
            for id in &ids[1..] {
                error = error.alternative(Suggestion::command(with_target(&ctx.argv, id)));
            }
            Err(error)
        }
    }
}

/// Errors that depend on the client: the addon may still load on another one.
fn client_specific(code: ErrorCode) -> bool {
    matches!(
        code,
        ErrorCode::NoCompatibleRelease | ErrorCode::WrongFlavor
    )
}

/// No live client loads `what`.
fn no_compatible(what: &str, live: &[&DiscoveredTarget]) -> Error {
    let checked: Vec<&str> = live.iter().map(|d| d.target.id.as_str()).collect();
    Error::new(
        ErrorCode::NoCompatibleRelease,
        format!(
            "no client here loads {what}: checked {}",
            checked.join(", ")
        ),
        Suggestion::command(["omaddons", "targets"]),
    )
    .details(json!({ "checked": checked }))
}

/// No live client: add one, or name a test client with `-t`.
fn no_live_target(ctx: &Context, targets: &[DiscoveredTarget]) -> Error {
    let mut tests: Vec<&str> = targets.iter().map(|d| d.target.id.as_str()).collect();
    tests.sort_unstable();
    let Some((first, rest)) = tests.split_first() else {
        return Error::new(
            ErrorCode::TargetNotFound,
            "no WoW client was found on this machine",
            Suggestion::text("omaddons targets add <path>"),
        );
    };
    let mut error = Error::new(
        ErrorCode::TargetNotFound,
        format!(
            "only test clients were found ({}); name one with -t",
            tests.join(", ")
        ),
        Suggestion::command(with_target(&ctx.argv, first)),
    )
    .details(json!({ "candidates": tests }));
    for id in rest {
        error = error.alternative(Suggestion::command(with_target(&ctx.argv, id)));
    }
    error
}

/// Each positional's result, in argument order, with the error of a failed one.
fn build_items(
    items: &[Item],
    done: &TxOutcomes,
    outcome: Option<&TxOutcome>,
    record: Option<&TargetRecord>,
    verbose: bool,
) -> Vec<(InstallItem, Option<Error>)> {
    let target_failure = done
        .failures
        .iter()
        .find(|f| f.r#ref.is_none())
        .and_then(|f| f.cause.clone());
    let asset_url = |release_id: Option<&String>| {
        let id = release_id.filter(|_| verbose)?;
        done.plan
            .targets
            .iter()
            .flat_map(|t| &t.releases)
            .find(|r| &r.release_id == id)?
            .url
            .clone()
    };
    let tp = done.plan.targets.first();
    // Each ref's first request, by identity: a later one collapsed into it.
    let mut seen: Vec<(String, String)> = Vec::new();
    items
        .iter()
        .map(|item| {
            let failed = |r: Option<Ref>, error: &Error| {
                let known = r.as_ref().and_then(|r| {
                    record.and_then(|rec| rec.addons.values().find(|a| a.r#ref == *r))
                });
                let item = InstallItem {
                    error: Some(ItemError::from(error)),
                    ..InstallItem::bare(item.request.clone(), r, ItemStatus::Failed)
                }
                .with_dependencies(known, None, false);
                (item, Some(error.clone()))
            };
            let p = match &item.parsed {
                Ok(p) => p,
                Err(error) => return failed(None, error),
            };
            let identity = p.r#ref.identity();
            if let Some((_, first)) = seen.iter().find(|(i, _)| *i == identity) {
                let duplicate = InstallItem {
                    skip_reason: Some(SkipReason::Duplicate),
                    duplicate_of: Some(first.clone()),
                    ..InstallItem::bare(
                        item.request.clone(),
                        Some(p.r#ref.clone()),
                        ItemStatus::Skipped,
                    )
                };
                return (duplicate, None);
            }
            seen.push((identity, item.request.clone()));
            if let Some(error) = done
                .failures
                .iter()
                .find(|f| f.r#ref.as_ref() == Some(&p.r#ref))
                .and_then(|f| f.cause.as_ref())
            {
                return failed(Some(p.r#ref.clone()), error);
            }
            let change = outcome.and_then(|o| o.changes.iter().find(|c| c.r#ref == p.r#ref));
            // Planned but not committed: the target's commit failed (or a concurrent run
            // already made the change, which is unchanged).
            let was_planned = done
                .plan
                .targets
                .iter()
                .flat_map(|t| &t.changes)
                .any(|c| c.r#ref == p.r#ref);
            if change.is_none()
                && was_planned
                && let Some(error) = &target_failure
            {
                return failed(Some(p.r#ref.clone()), error);
            }
            let planned = done
                .plan
                .targets
                .iter()
                .flat_map(|t| &t.work)
                .find_map(|w| w.record().filter(|r| r.r#ref == p.r#ref));
            let installed = record.and_then(|r| r.addons.values().find(|a| a.r#ref == p.r#ref));
            match change {
                Some(change) => {
                    if let Some(error) = &target_failure {
                        return failed(Some(p.r#ref.clone()), error);
                    }
                    // A records-only change: the record as planned says which way a pin
                    // went.
                    let pinned = planned.or(installed).is_some_and(|r| r.pinned);
                    let status = match change.kind {
                        ChangeKind::Update => ItemStatus::Updated,
                        ChangeKind::Edit if change.fields == [EditField::Pinned] && pinned => {
                            ItemStatus::Pinned
                        }
                        ChangeKind::Edit if change.fields == [EditField::Pinned] => {
                            ItemStatus::Unpinned
                        }
                        ChangeKind::Edit => ItemStatus::Edited,
                        ChangeKind::Install | ChangeKind::Remove | ChangeKind::Adopt => {
                            ItemStatus::Installed
                        }
                    };
                    let fields = match status {
                        ItemStatus::Edited => change.fields.clone(),
                        _ => Vec::new(),
                    };
                    let item = InstallItem {
                        name: Some(change.addon.clone()),
                        fields,
                        from_version: change.from_version.clone(),
                        to_version: change.to_version.clone(),
                        version_confidence: planned.map(|r| r.version_confidence),
                        channel: planned.map(|r| r.channel),
                        folders: sorted(&change.folders),
                        asset_url: asset_url(planned.and_then(|r| r.release_id.as_ref())),
                        ..InstallItem::bare(item.request.clone(), Some(p.r#ref.clone()), status)
                    }
                    .with_dependencies(planned.or(installed), tp, verbose);
                    (item, None)
                }
                None => {
                    let item = InstallItem {
                        name: installed.map(|a| a.name.clone()),
                        from_version: installed.map(|a| a.version.clone()),
                        to_version: installed.map(|a| a.version.clone()),
                        version_confidence: installed.map(|a| a.version_confidence),
                        channel: installed.map(|a| a.channel),
                        ..InstallItem::bare(
                            item.request.clone(),
                            Some(p.r#ref.clone()),
                            ItemStatus::Unchanged,
                        )
                    }
                    .with_dependencies(installed, tp, verbose);
                    (item, None)
                }
            }
        })
        .collect()
}

/// `items` with each requester's dependencies right after it, in resolution order (a
/// dependency's own follow it), every dependency once; then those of requesters no item
/// lists. Each is named by the folder that was required: installed or updated with its
/// change, or failed with its failure (`details.required_by`).
fn with_dependency_items(
    items: Vec<InstallItem>,
    done: &TxOutcomes,
    outcome: Option<&TxOutcome>,
    verbose: bool,
) -> Vec<InstallItem> {
    let Some(tp) = done.plan.targets.first() else {
        return items;
    };
    let target_failure = done.failures.iter().find(|f| f.r#ref.is_none());
    let planned = |addon: &str| {
        tp.work
            .iter()
            .filter(|w| w.addon.eq_ignore_ascii_case(addon))
            .find_map(|w| w.record())
    };
    let item_of = |dep: DepItem| -> Option<InstallItem> {
        match dep {
            DepItem::Planned { folder, addon } => {
                let record = planned(&addon);
                let request = folder.as_str().to_owned();
                let change = outcome.and_then(|o| {
                    o.changes
                        .iter()
                        .find(|c| c.addon.eq_ignore_ascii_case(&addon))
                });
                let item = match (change, target_failure) {
                    (Some(change), None) => {
                        let status = match change.kind {
                            ChangeKind::Update => ItemStatus::Updated,
                            _ => ItemStatus::Installed,
                        };
                        InstallItem {
                            name: Some(change.addon.clone()),
                            from_version: change.from_version.clone(),
                            to_version: change.to_version.clone(),
                            version_confidence: record.map(|r| r.version_confidence),
                            channel: record.map(|r| r.channel),
                            folders: sorted(&change.folders),
                            ..InstallItem::bare(request, Some(change.r#ref.clone()), status)
                        }
                    }
                    (_, Some(f)) => InstallItem {
                        name: Some(addon),
                        error: Some(ItemError::from(&f.error)),
                        ..InstallItem::bare(
                            request,
                            record.map(|r| r.r#ref.clone()),
                            ItemStatus::Failed,
                        )
                    },
                    // Dropped under the lock: a concurrent run made it.
                    (None, None) => return None,
                };
                Some(item.with_dependencies(record, Some(tp), verbose))
            }
            DepItem::Failed { folder, r#ref } => {
                let f = done
                    .failures
                    .iter()
                    .find(|f| f.r#ref.as_ref() == Some(&r#ref))?;
                let required_by = f.error.details["required_by"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect();
                Some(InstallItem {
                    name: f.addon.clone(),
                    error: Some(ItemError::from(&f.error)),
                    reason: Some(AddonReason::Dependency),
                    required_by,
                    ..InstallItem::bare(folder.as_str().to_owned(), Some(r#ref), ItemStatus::Failed)
                })
            }
        }
    };
    let mut listed: BTreeSet<String> = items
        .iter()
        .filter_map(|i| i.name.as_deref())
        .map(addon_identity)
        .chain(
            items
                .iter()
                .filter_map(|i| i.r#ref.as_ref())
                .map(Ref::identity),
        )
        .collect();
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        let requester = item.name.clone();
        out.push(item);
        if let Some(requester) = requester {
            let deps = dependency_items(&tp.dependencies, &requester, &mut listed);
            out.extend(deps.into_iter().filter_map(&item_of));
        }
    }
    for requester in tp.dependencies.keys() {
        let deps = dependency_items(&tp.dependencies, requester, &mut listed);
        out.extend(deps.into_iter().filter_map(&item_of));
    }
    out
}

/// At most three hints: `install <candidate> -t <id>` for the candidates of the items'
/// unresolved requirements in item order (not a project that does not ship the folder
/// here), then each `bridge_dependency` warning's fix, which installs the requirement or,
/// when nothing resolves it, searches for it. Each is labelled by the verb its argv runs
/// (`install` or `search`), never by where it came from.
fn install_hints(items: &[InstallItem], warnings: &[Warning], target: &str) -> Vec<CommandHint> {
    let candidates = items
        .iter()
        .flat_map(|i| &i.unresolved)
        .filter(|u| u.reason != UnresolvedReason::NotShipped)
        .flat_map(|u| &u.candidates)
        .map(|c| {
            ["omaddons", "install", &c.to_string(), "-t", target]
                .map(str::to_owned)
                .to_vec()
        });
    let bridges = warnings
        .iter()
        .filter(|w| matches!(w.code, WarningCode::BridgeDependency))
        .filter_map(|w| w.fix.as_ref()?.argv().map(<[String]>::to_vec));
    let mut argvs: Vec<Vec<String>> = Vec::new();
    for argv in candidates.chain(bridges) {
        if argvs.len() < INSTALL_HINTS_MAX && !argvs.contains(&argv) {
            argvs.push(argv);
        }
    }
    argvs
        .into_iter()
        .map(|argv| {
            let verb = argv.get(1).cloned().unwrap_or_default();
            CommandHint::new(verb, argv, false)
        })
        .collect()
}

/// How many install (or search) hints a run suggests at most.
const INSTALL_HINTS_MAX: usize = 3;

/// `a`, `a and b`, `a, b and c`.
fn and_list(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [only] => only.clone(),
        [init @ .., last] => format!("{} and {last}", init.join(", ")),
    }
}

/// The mode `selector` names in `target`'s client, unless it names the target itself.
fn mode_note(selector: &str, target: &DiscoveredTarget, table: &RulesTable) -> Option<ModeNote> {
    if target.target.id.eq_ignore_ascii_case(selector) {
        return None;
    }
    let product = table.product_by_code(target.target.product.as_deref()?)?;
    let mode = product.modes.iter().find(|m| m.named(selector))?;
    Some(ModeNote {
        id: mode.id.clone(),
        name: mode.name.clone(),
        shared_with: product
            .modes
            .iter()
            .filter(|m| m.id != mode.id)
            .map(|m| m.name.clone())
            .collect(),
    })
}

/// Every item failed: the first item's error, with every item's error in details.
pub(super) fn all_failed(errors: &[(String, Error)]) -> Error {
    let Some((_, first)) = errors.first() else {
        return Error::internal("install without items");
    };
    let listed: Vec<serde_json::Value> = errors
        .iter()
        .map(|(request, e)| json!({ "request": request, "code": e.code.wire(), "message": e.message }))
        .collect();
    let mut details = first.details.clone();
    if let Some(map) = details.as_object_mut() {
        map.insert("items".into(), json!(listed));
    }
    first.clone().details(details)
}

fn usage(message: &str) -> Error {
    Error::new(
        ErrorCode::Usage,
        message,
        Suggestion::command(["omaddons", "help", "install"]),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::inspect::SkippedFolder;
    use crate::model::FolderName;
    use crate::toc::WrongFlavorReason;

    fn item(request: &str, status: ItemStatus, version: Option<&str>) -> InstallItem {
        let failed = status == ItemStatus::Failed;
        InstallItem {
            name: (!failed).then(|| request.to_owned()),
            to_version: version.map(str::to_owned),
            folders: FolderSummary {
                added: vec![FolderName::new(request).unwrap()],
                ..FolderSummary::default()
            },
            error: failed.then(|| {
                ItemError::from(&Error::new(
                    ErrorCode::NoCompatibleRelease,
                    "no release verifies for mainline 120105",
                    Suggestion::text("x"),
                ))
            }),
            ..InstallItem::bare(request.into(), None, status)
        }
    }

    fn outcome(
        items: Vec<InstallItem>,
        tx: Option<TxId>,
        dry_run: bool,
        strict: bool,
    ) -> InstallOutcome {
        InstallOutcome {
            data: InstallData {
                dry_run,
                target: TargetRef {
                    id: "retail".into(),
                    key: None,
                    mode: None,
                },
                tx,
                kind: None,
                items,
                reload_required: ReloadRequired::Reload,
                atomic_swap: None,
                creates_addons_dir: false,
                client_dir: None,
            },
            warnings: Vec::new(),
            commands: Vec::new(),
            strict,
            reinstall: false,
            from_cache: Vec::new(),
            lock_held_ms: None,
            notes: Vec::new(),
        }
    }

    fn human(o: &InstallOutcome) -> String {
        let mut out = Vec::new();
        let style = Style {
            color: false,
            verbose: 0,
            quiet: false,
        };
        o.render_human(&mut out, &style).unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn every_item_failing_is_the_first_error_listing_each() {
        let errors = vec![
            (
                "gh:o/bar".to_owned(),
                Error::new(
                    ErrorCode::NoCompatibleRelease,
                    "none",
                    Suggestion::text("x"),
                ),
            ),
            (
                "gh:".to_owned(),
                Error::new(ErrorCode::RefInvalid, "bad", Suggestion::text("y")),
            ),
        ];
        let error = all_failed(&errors);
        assert_eq!(
            (error.code, error.exit().code()),
            (ErrorCode::NoCompatibleRelease, 3)
        );
        assert_eq!(
            error.details["items"],
            json!([
                {"request": "gh:o/bar", "code": "no_compatible_release", "message": "none"},
                {"request": "gh:", "code": "ref_invalid", "message": "bad"},
            ])
        );
    }

    #[test]
    fn strict_turns_a_partial_success_into_check_failed() {
        let mixed = || {
            vec![
                item("Foo", ItemStatus::Installed, Some("1.0")),
                item("Bar", ItemStatus::Failed, None),
            ]
        };
        let lenient = outcome(mixed(), None, false, false);
        assert!(lenient.partial() && lenient.assertion_failure().is_none());
        let strict = outcome(mixed(), None, false, true);
        let failure = strict.assertion_failure().unwrap();
        assert_eq!(
            (failure.code, failure.exit().code()),
            (ErrorCode::CheckFailed, 6)
        );
        let clean = outcome(
            vec![item("Foo", ItemStatus::Unchanged, Some("1.0"))],
            None,
            false,
            true,
        );
        assert!(!clean.partial() && clean.assertion_failure().is_none());
    }

    #[test]
    fn human_lines() {
        let tx: TxId = "01JB8Q2VA00000000000000000".parse().unwrap();
        let o = outcome(
            vec![
                item("WeakAuras", ItemStatus::Installed, Some("5.19.2")),
                item("DBM", ItemStatus::Unchanged, Some("11.2.5")),
                item("BigWigs", ItemStatus::Failed, None),
            ],
            Some(tx),
            false,
            false,
        );
        assert_eq!(
            human(&o),
            "retail: installed WeakAuras 5.19.2 (1 folder)\n\
             retail: DBM unchanged (11.2.5)\n\
             retail: failed BigWigs: no_compatible_release: no release verifies for mainline 120105\n\
             tx 01JB8Q2VA00000000000000000 · /reload in game · undo: omaddons undo 01JB8Q2VA00000000000000000\n"
        );
        let dry = outcome(
            vec![item("Foo", ItemStatus::Updated, Some("2.0"))],
            None,
            true,
            false,
        );
        assert_eq!(
            human(&dry),
            "retail: would update Foo 2.0 (1 folder)\n/reload in game\n"
        );
    }

    #[test]
    fn records_only_items_name_the_change() {
        let tx: TxId = "01JB8Q2VA00000000000000000".parse().unwrap();
        let mut edited = item("Bar", ItemStatus::Edited, Some("2.0"));
        edited.fields = vec![EditField::Channel, EditField::Reason];
        let mut o = outcome(
            vec![
                item("Foo", ItemStatus::Pinned, Some("1.2")),
                item("Baz", ItemStatus::Unpinned, Some("3")),
                edited,
            ],
            Some(tx),
            false,
            false,
        );
        o.data.reload_required = ReloadRequired::None;
        assert_eq!(
            human(&o),
            "retail: pinned Foo at 1.2\n\
             retail: unpinned Baz at 3\n\
             retail: edited Bar at 2.0 (channel, reason)\n\
             tx 01JB8Q2VA00000000000000000 · undo: omaddons undo 01JB8Q2VA00000000000000000\n"
        );
        o.data.dry_run = true;
        o.data.tx = None;
        assert!(human(&o).starts_with("retail: would pin Foo at 1.2\n"));
        let json = serde_json::to_value(&o.data.items).unwrap();
        assert_eq!(
            (&json[0]["status"], &json[2]["status"], &json[2]["fields"]),
            (
                &json!("pinned"),
                &json!("edited"),
                &json!(["channel", "reason"])
            )
        );
        assert!(json[0].get("fields").is_none(), "omitted when empty");
    }

    #[test]
    fn skipped_folders_get_one_line_per_addon() {
        let skip = |n: &str| SkippedFolder {
            name: FolderName::new(n).unwrap(),
            reason: WrongFlavorReason::GameTypeExcluded,
        };
        let mut dbm = item("DBM-Core", ItemStatus::Installed, Some("12.1.14"));
        dbm.folders.skipped = vec![skip("DBM-Brawlers"), skip("DBM-Midnight")];
        let mut one = item("Foo", ItemStatus::Installed, Some("1.0"));
        one.folders.skipped = vec![skip("Foo_Retail")];
        assert_eq!(
            human(&outcome(vec![dbm.clone(), one], None, false, false)),
            "retail: installed DBM-Core 12.1.14 (1 folder)\n  \
             skipped 2 folders this client does not load: DBM-Brawlers, DBM-Midnight\n\
             retail: installed Foo 1.0 (1 folder)\n  \
             skipped 1 folder this client does not load: Foo_Retail\n\
             /reload in game\n"
        );
        assert!(
            human(&outcome(vec![dbm], None, true, false))
                .contains("  would skip 2 folders this client does not load:")
        );
    }

    #[test]
    fn creating_the_addons_folder_is_said_first() {
        for (dry_run, verb) in [(true, "would create"), (false, "created")] {
            let mut o = outcome(
                vec![item("Foo", ItemStatus::Installed, Some("1.0"))],
                None,
                dry_run,
                false,
            );
            o.data.creates_addons_dir = true;
            let text = human(&o);
            assert!(
                text.starts_with(&format!(
                    "retail: {verb} Interface/AddOns (this client has not been launched yet)\n"
                )),
                "{text}"
            );
        }
    }

    #[test]
    fn only_selection_and_verification_depend_on_the_client() {
        assert!(client_specific(ErrorCode::NoCompatibleRelease));
        assert!(client_specific(ErrorCode::WrongFlavor));
        for code in [
            ErrorCode::NotFound,
            ErrorCode::BadArchive,
            ErrorCode::UnmanagedFolder,
        ] {
            assert!(!client_specific(code), "{code}");
        }
    }

    #[test]
    fn folders_are_sorted_by_key() {
        let f = |n: &str| FolderName::new(n).unwrap();
        let skip = |n: &str| SkippedFolder {
            name: f(n),
            reason: WrongFlavorReason::GameTypeExcluded,
        };
        let s = sorted(&FolderSummary {
            added: vec![f("b"), f("A"), f("c")],
            replaced: Vec::new(),
            removed: vec![f("Z"), f("y")],
            skipped: vec![skip("m"), skip("K")],
            recorded: vec![f("Q"), f("p")],
        });
        assert_eq!(s.added, [f("A"), f("b"), f("c")]);
        assert_eq!(s.removed, [f("y"), f("Z")]);
        assert_eq!(s.skipped, [skip("K"), skip("m")]);
        assert_eq!(s.recorded, [f("p"), f("Q")]);
    }
}
