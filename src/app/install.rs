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

use std::collections::BTreeMap;
use std::io::Write;
use std::path::PathBuf;

use schemars::JsonSchema;
use serde::Serialize;
use serde_json::json;

use super::engine::Engine;
use super::world::World;
use crate::cli::Context;
use crate::discover::DiscoveredTarget;
use crate::discover::registry::{resolve_one, with_target};
use crate::engine::plan::{ChangeKind, FolderSummary, PinIntent, RequestedChange};
use crate::engine::{TxOutcome, TxOutcomes, TxRequest, TxTarget};
use crate::error::{CommandHint, Error, ErrorCode, Result, Suggestion, Warning};
use crate::model::{
    AddonReason, Channel, FolderName, Ref, ReloadRequired, TargetKey, TxId, TxKind,
    VersionConfidence,
};
use crate::output::{Outcome, Style};
use crate::resolve::catalog::Catalog;
use crate::rules::{RulesTable, TargetKind};
use crate::sources::refs::{CatalogMatch, InstalledMatch, ParsedRef, RefContext, parse_user_ref};
use crate::state::records::{TargetRecord, load_record};

/// `install`'s own flags.
#[derive(Debug, Clone, Default, clap::Args)]
pub struct InstallArgs {
    /// What to install: a catalog name (weakauras), owner/repo, a canonical ref (gh:, git:,
    /// url:, file:), a pasted GitHub or https link, or a path to a zip or folder
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
    /// Accepted; dependencies arrive in a later release
    #[arg(long)]
    pub no_deps: bool,
    /// Take over a folder another addon owns, or replace an unmanaged folder
    #[arg(long)]
    pub force: bool,
    /// Exit 6 when only some of the addons install
    #[arg(long)]
    pub strict: bool,
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

/// The target acted on.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct TargetRef {
    pub id: String,
    /// `null` only under `--dry-run` on an unregistered target.
    pub key: Option<TargetKey>,
    /// Present when `-t` named a game mode of the client (`-t sod`) rather than the
    /// target: the addons go to the client, which every mode in it shares.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<ModeNote>,
}

/// A game mode `-t` named, and the client's other modes, which share its addons.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ModeNote {
    pub id: String,
    pub name: String,
    /// The other modes' names, e.g. `["Classic Era", "Hardcore"]`.
    pub shared_with: Vec<String>,
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
    pub from_version: Option<String>,
    pub to_version: Option<String>,
    pub version_confidence: Option<VersionConfidence>,
    pub channel: Option<Channel>,
    pub folders: FolderSummary,
    pub error: Option<ItemError>,
    /// `-v` only: where the installed release came from.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub asset_url: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ItemStatus {
    Installed,
    Updated,
    Unchanged,
    Failed,
}

/// A failed item's error.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ItemError {
    pub code: ErrorCode,
    pub message: String,
    pub fix: String,
    pub fix_argv: Option<Vec<String>>,
}

impl From<&Error> for ItemError {
    fn from(e: &Error) -> Self {
        Self {
            code: e.code,
            message: e.message.clone(),
            fix: e.fix.display().to_owned(),
            fix_argv: e.fix.argv().map(<[String]>::to_vec),
        }
    }
}

/// `install`'s outcome.
#[derive(Debug)]
pub struct InstallOutcome {
    data: InstallData,
    warnings: Vec<Warning>,
    commands: Vec<CommandHint>,
    strict: bool,
    /// The global lock's hold time, for the `-v` log line.
    pub lock_held_ms: Option<u64>,
    /// Why addons were left unchanged, for `-v` log lines.
    pub notes: Vec<String>,
}

impl InstallOutcome {
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
            match item.status {
                ItemStatus::Installed | ItemStatus::Updated => {
                    let verb = if item.status == ItemStatus::Installed {
                        "installed"
                    } else {
                        "updated"
                    };
                    let verb = if d.dry_run {
                        format!("would be {verb}")
                    } else {
                        verb.to_owned()
                    };
                    writeln!(
                        w,
                        "{id}: {verb} {name} {} ({folders} folders)",
                        item.to_version.as_deref().unwrap_or("?")
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
                ItemStatus::Failed => {
                    let (code, message) = item
                        .error
                        .as_ref()
                        .map_or(("", ""), |e| (e.code.wire(), e.message.as_str()));
                    writeln!(w, "{id}: failed {name}: {code}: {message}")?;
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
    let world = World::load()?;
    let mut warnings = world.warnings.clone();
    let (catalog, catalog_warnings) = Catalog::load(&world.paths);
    warnings.extend(catalog_warnings);
    let records = world.records()?;

    // Parse every positional; parse failures are failed items.
    let items: Vec<Item> = args
        .refs
        .iter()
        .map(|request| Item {
            request: request.clone(),
            parsed: parse_ref(&world, &records, &catalog, &cwd, request),
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
        RequestedChange::Install {
            r#ref: p.r#ref.clone(),
            channel: channel
                .or_else(|| recorded_channel(&records, target, &p.r#ref))
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
            // A catalog ref is recorded under the catalog's name, however it was named.
            name: catalog
                .entries()
                .iter()
                .find(|e| e.r#ref == p.r#ref)
                .map(|e| e.name.clone()),
            // Asking for an addon by name makes it explicit, promoting a dependency.
            reason: AddonReason::Explicit,
            reinstall: false,
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
    let target = choose_target(ctx, &world, &engine, &items, &change_for)?;

    let req = TxRequest {
        targets: vec![TxTarget {
            discovered: target.clone(),
            changes: parsed.iter().map(|p| change_for(p, target)).collect(),
            default_kind: TxKind::Install,
        }],
        force: args.force,
        while_running: ctx.while_running,
        dry_run: ctx.dry_run,
        reason: ctx.reason.clone(),
        kind: None,
    };
    let done = engine.run(&req)?;
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
    let built = build_items(&items, &done, outcome, after.as_ref(), verbose);
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
    let commands = tx
        .map(|tx| {
            vec![CommandHint::new(
                "undo",
                ["omaddons".to_owned(), "undo".to_owned(), tx.to_string()],
                false,
            )]
        })
        .unwrap_or_default();
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
        lock_held_ms: outcome.and_then(|o| o.lock_held_ms),
        notes: outcome.map(|o| o.notes.clone()).unwrap_or_default(),
    })
}

/// Parses what a user typed (a canonical ref, a pasted URL, a path, an installed
/// addon's name or folder, a catalog name or alias) against `records` and `catalog`.
pub(super) fn parse_ref(
    world: &World,
    records: &BTreeMap<String, TargetRecord>,
    catalog: &Catalog,
    cwd: &std::path::Path,
    request: &str,
) -> std::result::Result<ParsedRef, Error> {
    let installed = |name: &str| -> Vec<InstalledMatch> {
        let mut found: Vec<InstalledMatch> = Vec::new();
        for (target_id, record) in records {
            for addon in record.addons.values() {
                let by_folder = addon
                    .folders
                    .iter()
                    .any(|f| f.as_str().eq_ignore_ascii_case(name));
                if (addon.name.eq_ignore_ascii_case(name) || by_folder)
                    && !found.iter().any(|m| m.r#ref == addon.r#ref)
                {
                    found.push(InstalledMatch {
                        name: addon.name.clone(),
                        r#ref: addon.r#ref.clone(),
                        target_id: target_id.clone(),
                    });
                }
            }
        }
        found
    };
    let in_catalog = |name: &str| {
        catalog.by_name(name).map(|e| CatalogMatch {
            name: e.name.clone(),
            r#ref: e.r#ref.clone(),
        })
    };
    let ref_ctx = RefContext {
        cwd: cwd.to_path_buf(),
        git_hosts: &world.config.git_hosts,
        installed: &installed,
        catalog: &in_catalog,
    };
    parse_user_ref(request, &ref_ctx)
}

/// The channel an addon was installed with on `target`.
fn recorded_channel(
    records: &BTreeMap<String, TargetRecord>,
    target: &DiscoveredTarget,
    r: &Ref,
) -> Option<Channel> {
    records
        .get(&target.target.id)?
        .addons
        .values()
        .find(|a| a.r#ref == *r)
        .map(|a| a.channel)
}

/// `-t`, else the configured default, else the single live target every addon
/// verifies on. Test clients (PTR, beta) are only ever chosen with `-t`.
fn choose_target<'w>(
    ctx: &Context,
    world: &'w World,
    engine: &Engine,
    items: &[Item],
    change_for: &dyn Fn(&ParsedRef, &DiscoveredTarget) -> RequestedChange,
) -> Result<&'w DiscoveredTarget> {
    let targets = &world.resolved.targets;
    if let Some(selector) = &ctx.target {
        return resolve_one(selector, targets, &world.table, &ctx.argv);
    }
    if let Some(selector) = &world.config.default_target {
        return resolve_one(selector, targets, &world.table, &ctx.argv);
    }
    if let Some(key) = world.registry.default
        && let Some(found) = targets.iter().find(|d| d.target.key == Some(key))
    {
        return Ok(found);
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
    // (downloads land in the blob cache the real run then reuses).
    let parsed: Vec<&ParsedRef> = items
        .iter()
        .filter_map(|i| i.parsed.as_ref().ok())
        .collect();
    let probe = TxRequest {
        targets: live
            .iter()
            .map(|d| TxTarget {
                discovered: (*d).clone(),
                changes: parsed.iter().map(|p| change_for(p, d)).collect(),
                default_kind: TxKind::Install,
            })
            .collect(),
        force: false,
        while_running: true,
        dry_run: true,
        reason: None,
        kind: None,
    };
    let probed = engine.run(&probe)?;
    let failure = |d: &DiscoveredTarget, r: &Ref| {
        probed
            .failures
            .iter()
            .find(|f| f.target == d.target.id && f.r#ref.as_ref() == Some(r))
            .and_then(|f| f.cause.as_ref())
    };
    // An addon that fails on every client cannot choose one.
    let fails_everywhere = |p: &ParsedRef| live.iter().all(|d| failure(d, &p.r#ref).is_some());
    if parsed.iter().all(|p| fails_everywhere(p)) {
        let errors: Vec<(String, Error)> = items
            .iter()
            .map(|i| {
                let error = match &i.parsed {
                    Err(e) => e.clone(),
                    Ok(p) => live
                        .iter()
                        .filter_map(|d| failure(d, &p.r#ref))
                        .find(|e| !client_specific(e.code))
                        .cloned()
                        .unwrap_or_else(|| no_compatible(&i.request, &live)),
                };
                (i.request.clone(), error)
            })
            .collect();
        return Err(all_failed(&errors));
    }
    let deciding: Vec<&ParsedRef> = parsed
        .iter()
        .copied()
        .filter(|p| !fails_everywhere(p))
        .collect();
    let compatible: Vec<&DiscoveredTarget> = live
        .iter()
        .copied()
        .filter(|d| {
            deciding
                .iter()
                .all(|p| failure(d, &p.r#ref).is_none_or(|e| !client_specific(e.code)))
        })
        .collect();
    let requests = items
        .iter()
        .filter(|i| i.parsed.as_ref().is_ok_and(|p| deciding.contains(&p)))
        .map(|i| i.request.as_str())
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
    items
        .iter()
        .map(|item| {
            let failed = |r: Option<Ref>, error: &Error| {
                let item = InstallItem {
                    request: item.request.clone(),
                    r#ref: r,
                    name: None,
                    status: ItemStatus::Failed,
                    from_version: None,
                    to_version: None,
                    version_confidence: None,
                    channel: None,
                    folders: FolderSummary::default(),
                    error: Some(ItemError::from(error)),
                    asset_url: None,
                };
                (item, Some(error.clone()))
            };
            let p = match &item.parsed {
                Ok(p) => p,
                Err(error) => return failed(None, error),
            };
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
                    let item = InstallItem {
                        request: item.request.clone(),
                        r#ref: Some(p.r#ref.clone()),
                        name: Some(change.addon.clone()),
                        status: if change.kind == ChangeKind::Update {
                            ItemStatus::Updated
                        } else {
                            ItemStatus::Installed
                        },
                        from_version: change.from_version.clone(),
                        to_version: change.to_version.clone(),
                        version_confidence: planned.map(|r| r.version_confidence),
                        channel: planned.map(|r| r.channel),
                        folders: sorted(&change.folders),
                        error: None,
                        asset_url: asset_url(planned.and_then(|r| r.release_id.as_ref())),
                    };
                    (item, None)
                }
                None => {
                    let item = InstallItem {
                        request: item.request.clone(),
                        r#ref: Some(p.r#ref.clone()),
                        name: installed.map(|a| a.name.clone()),
                        status: ItemStatus::Unchanged,
                        from_version: installed.map(|a| a.version.clone()),
                        to_version: installed.map(|a| a.version.clone()),
                        version_confidence: installed.map(|a| a.version_confidence),
                        channel: installed.map(|a| a.channel),
                        folders: FolderSummary::default(),
                        error: None,
                        asset_url: None,
                    };
                    (item, None)
                }
            }
        })
        .collect()
}

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

/// Folder lists sorted by key.
fn sorted(folders: &FolderSummary) -> FolderSummary {
    let sort = |list: &Vec<FolderName>| {
        let mut list = list.clone();
        list.sort_by(|a, b| a.key().cmp(b.key()));
        list
    };
    let mut skipped = folders.skipped.clone();
    skipped.sort_by(|a, b| a.name.key().cmp(b.name.key()));
    FolderSummary {
        added: sort(&folders.added),
        replaced: sort(&folders.replaced),
        removed: sort(&folders.removed),
        skipped,
        recorded: sort(&folders.recorded),
    }
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
    use crate::toc::WrongFlavorReason;

    fn item(request: &str, status: ItemStatus, version: Option<&str>) -> InstallItem {
        let failed = status == ItemStatus::Failed;
        InstallItem {
            request: request.into(),
            r#ref: None,
            name: (!failed).then(|| request.to_owned()),
            status,
            from_version: None,
            to_version: version.map(str::to_owned),
            version_confidence: None,
            channel: None,
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
            asset_url: None,
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
            "retail: installed WeakAuras 5.19.2 (1 folders)\n\
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
            "retail: would be updated Foo 2.0 (1 folders)\n/reload in game\n"
        );
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
            "retail: installed DBM-Core 12.1.14 (1 folders)\n  \
             skipped 2 folders this client does not load: DBM-Brawlers, DBM-Midnight\n\
             retail: installed Foo 1.0 (1 folders)\n  \
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
