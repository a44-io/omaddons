//! `omaddons outdated [-t T] [--refresh] [--check] [--verify]` (alias `check`): which
//! managed addons have updates, on every client, without changing anything.
//!
//! Each distinct ref and channel is fetched at most once, through the release memo: a
//! check from the last 15 minutes is reused (`--refresh` fetches anyway; `--offline`
//! answers from the memo whatever its age). Every status comes from the shared
//! evaluator (`resolve::updates`), so `outdated` and `update` never disagree. Static
//! installs (local files, URLs, given archives) make no request.
//!
//! The listing is token-frugal: only what needs a decision (updates, pins with a newer
//! release, blocked, ahead, failed) unless `-v`; the summary counts everything.
//! `--verify` downloads and verifies the updates not yet known to load, as installing
//! them would, in a dry run that takes no global lock; the facts it learns are
//! remembered, so the same run (and every later one) shows them verified or blocked.
//! It writes only the memo and the blob cache, `net.log` and, with `--verify`, an
//! in-flight lock.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;

use jiff::Timestamp;
use schemars::JsonSchema;
use serde::Serialize;

use super::engine::Engine;
use super::outcome::TargetRef;
use super::status::modes_or_family;
use super::world::World;
use crate::cli::Context;
use crate::contract::actions::Action;
use crate::discover::DiscoveredTarget;
use crate::engine::plan::RequestedChange;
use crate::engine::{TxRequest, TxTarget};
use crate::error::{
    CommandHint, Error, ErrorBody, ErrorCode, Result, Suggestion, Warning, WarningCode,
};
use crate::model::{Channel, Ref, ReleaseSet, RunId, Target, TxKind};
use crate::output::{Outcome, Phase, Style};
use crate::politeness::SOURCE_RECHECK_MIN_INTERVAL;
use crate::resolve::cache::{Fetched, Freshness, Origin, ReleaseCache};
use crate::resolve::updates::{
    BlockedReason, Confidence, ReleaseBrief, StatusKind, StatusRow, UncheckedReason, UpdateStatus,
    installed_primary_interface, is_static, statuses, update_floor,
};
use crate::sources::changelog::{
    Changelog, ChangelogUnavailable, Installed as ChangelogInstalled, Notes, changelog_for,
    human_lines, identity, newer_than, oldest_since,
};
use crate::sources::{SourceId, Wanted};
use crate::state::memo::{CandidateFact, FactIndex};
use crate::state::records::TargetRecord;

/// `outdated`'s own arguments.
#[derive(Debug, Clone, Default, clap::Args)]
pub struct OutdatedArgs {
    /// Ask every source now, even when it was checked in the last 15 minutes
    #[arg(long)]
    pub refresh: bool,
    /// Exit 6 (check_failed) when any update is available
    #[arg(long)]
    pub check: bool,
    /// Download and verify each update not yet known to load, as installing it would
    #[arg(long)]
    pub verify: bool,
    /// Add each update's release notes since the installed release
    #[arg(long)]
    pub changelog: bool,
}

/// `outdated`'s data.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct OutdatedData {
    /// When the oldest release metadata this check used was fetched; `null` when nothing
    /// was checked.
    #[schemars(with = "Option<String>")]
    pub checked_at: Option<Timestamp>,
    pub requests: Requests,
    /// Every selected target, sorted by id.
    pub targets: Vec<OutdatedTarget>,
    /// Every item's status, listed or not.
    pub summary: Summary,
}

/// The distinct refs checked, by where their answer came from.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Requests {
    /// Fetched from the source now.
    pub fetched: usize,
    /// A remembered answer a cheap probe confirmed.
    pub probed: usize,
    /// Answered from the release memo without a request.
    pub reused: usize,
}

/// One target's items.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct OutdatedTarget {
    pub target: TargetRef,
    /// Sorted by name, ignoring case: the actionable ones, or every one with `-v`.
    pub items: Vec<OutdatedItem>,
}

/// One managed addon's update status on one target.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct OutdatedItem {
    pub name: String,
    #[serde(rename = "ref")]
    pub r#ref: Ref,
    pub channel: Channel,
    pub pinned: bool,
    pub installed: Installed,
    /// `null` for `failed`, `unchecked` and `static`.
    pub latest: Option<LatestRelease>,
    pub status: StatusKind,
    /// `update_available` only.
    pub confidence: Option<Confidence>,
    pub blocked_reason: Option<BlockedReason>,
    pub unchecked_reason: Option<UncheckedReason>,
    pub error: Option<ErrorBody>,
}

/// The newest release an item is judged against; with `--changelog`, an
/// `update_available` item's notes since its installed release.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct LatestRelease {
    #[serde(flatten)]
    pub release: ReleaseBrief,
    /// `--changelog`, `update_available` only: the notes, or `null`. Omitted without
    /// `--changelog` and for other statuses.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub changelog: Option<Option<Changelog>>,
    /// Why `changelog` is `null`; omitted when `changelog` is absent or holds notes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub changelog_unavailable: Option<ChangelogUnavailable>,
}

/// The installed release.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct Installed {
    pub version: String,
    pub release_id: Option<String>,
}

/// How many items have each status.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Summary {
    pub update_available: usize,
    pub current: usize,
    pub pinned: usize,
    pub ahead: usize,
    pub blocked: usize,
    pub failed: usize,
    pub unchecked: usize,
    #[serde(rename = "static")]
    pub r#static: usize,
}

impl Summary {
    fn count(&mut self, kind: StatusKind) {
        let n = match kind {
            StatusKind::UpdateAvailable => &mut self.update_available,
            StatusKind::Current => &mut self.current,
            StatusKind::Pinned => &mut self.pinned,
            StatusKind::Ahead => &mut self.ahead,
            StatusKind::Blocked => &mut self.blocked,
            StatusKind::Failed => &mut self.failed,
            StatusKind::Unchecked => &mut self.unchecked,
            StatusKind::Static => &mut self.r#static,
        };
        *n += 1;
    }
}

/// `outdated`'s outcome.
#[derive(Debug)]
pub struct OutdatedOutcome {
    data: OutdatedData,
    warnings: Vec<Warning>,
    commands: Vec<CommandHint>,
    partial: bool,
    check: bool,
    /// Per target, in `data.targets` order: `retail (12.1.5, mainline)`.
    headings: Vec<String>,
    /// How old `checked_at` is, in words (`just now`, `5 min ago`).
    age: Option<String>,
    /// For `-v` log lines: the memo ignored or not written, verification refusals.
    pub notes: Vec<String>,
}

impl Outcome for OutdatedOutcome {
    type Data = OutdatedData;

    fn command(&self) -> &'static str {
        "outdated"
    }

    fn data(&self) -> &OutdatedData {
        &self.data
    }

    fn warnings(&self) -> &[Warning] {
        &self.warnings
    }

    fn commands(&self) -> &[CommandHint] {
        &self.commands
    }

    fn partial(&self) -> bool {
        self.partial
    }

    fn assertion_failure(&self) -> Option<Error> {
        let updates = self.data.summary.update_available;
        if !self.check || updates == 0 {
            return None;
        }
        let fix = self.commands.first().map_or_else(
            || Suggestion::command(["omaddons", "outdated"]),
            |c| Suggestion::command(c.argv().to_vec()),
        );
        Some(Error::new(
            ErrorCode::CheckFailed,
            format!("{updates} {} available", plural(updates, "update")),
            fix,
        ))
    }

    fn render_human(&self, w: &mut dyn Write, _style: &Style) -> std::io::Result<()> {
        let items = || self.data.targets.iter().flat_map(|t| &t.items);
        let name_width = items().map(|i| i.name.chars().count()).max().unwrap_or(0);
        let installed_width = items()
            .map(|i| i.installed.version.chars().count())
            .max()
            .unwrap_or(0);
        let latest_width = items()
            .filter(|i| i.status == StatusKind::UpdateAvailable)
            .filter_map(|i| i.latest.as_ref().map(|l| l.release.shown().chars().count()))
            .max()
            .unwrap_or(0);
        // A ref is named on its first update line only: only those lines show it, so a
        // line of another status never uses its first mention up.
        let mut named: BTreeSet<String> = BTreeSet::new();
        for (t, heading) in self.data.targets.iter().zip(&self.headings) {
            if t.items.is_empty() {
                continue;
            }
            writeln!(w, "{heading}")?;
            for i in &t.items {
                let r = i.r#ref.to_string();
                let first = i.status == StatusKind::UpdateAvailable && named.insert(r.clone());
                let what = describe(i, latest_width, first.then_some(r.as_str()));
                let line = format!(
                    "  {:<name_width$}  {:<installed_width$}  {what}",
                    i.name, i.installed.version
                );
                writeln!(w, "{}", line.trim_end())?;
                // `--changelog`: the notes under the item.
                if let Some(l) = &i.latest
                    && let Some(changelog) = &l.changelog
                {
                    for line in human_lines(changelog.as_ref(), l.changelog_unavailable) {
                        writeln!(w, "      {line}")?;
                    }
                }
            }
        }
        writeln!(w, "{}", self.summary_line())?;
        // By action: the update hint (when there is one) comes first, then any adopt
        // hints, which install nothing.
        for hint in &self.commands {
            match hint.action() {
                Action::Update => writeln!(w, "Run `{}` to install them.", hint.command())?,
                Action::Adopt => writeln!(
                    w,
                    "Run `{}` to see the folders installed by hand that omaddons can manage.",
                    hint.command()
                )?,
                _ => {}
            }
        }
        Ok(())
    }
}

impl OutdatedOutcome {
    /// `3 updates on 2 clients · 1 pinned · checked just now (4 requests, 11 reused)`.
    fn summary_line(&self) -> String {
        let s = &self.data.summary;
        let with_updates = self
            .data
            .targets
            .iter()
            .filter(|t| {
                t.items
                    .iter()
                    .any(|i| i.status == StatusKind::UpdateAvailable)
            })
            .count();
        let clients = self.data.targets.len();
        let mut parts = vec![if s.update_available == 0 {
            format!("No updates on {clients} {}", plural(clients, "client"))
        } else {
            format!(
                "{} {} on {with_updates} {}",
                s.update_available,
                plural(s.update_available, "update"),
                plural(with_updates, "client")
            )
        }];
        for (n, what) in [
            (s.pinned, "pinned"),
            (s.blocked, "blocked"),
            (s.ahead, "ahead"),
            (s.failed, "failed"),
            (s.unchecked, "not checked"),
        ] {
            if n > 0 {
                parts.push(format!("{n} {what}"));
            }
        }
        let r = self.data.requests;
        let made = r.fetched + r.probed;
        parts.push(match &self.age {
            Some(age) => format!(
                "checked {age} ({made} {}, {} reused)",
                plural(made, "request"),
                r.reused
            ),
            None => "nothing checked".to_owned(),
        });
        parts.join(" · ")
    }
}

/// What an item's line says after its name and installed version.
fn describe(i: &OutdatedItem, latest_width: usize, r: Option<&str>) -> String {
    let shown = i.latest.as_ref().map(|l| l.release.shown());
    let latest = shown.as_deref().unwrap_or("?");
    match i.status {
        StatusKind::UpdateAvailable => {
            let doubt = match i.confidence {
                Some(Confidence::Unlikely) => " (may not load here)",
                _ => "",
            };
            format!(
                "→ {latest:<latest_width$}  {}{doubt}",
                r.unwrap_or_default()
            )
        }
        StatusKind::Pinned => format!("pinned (latest {latest})"),
        StatusKind::Ahead => format!("ahead of the newest release, {latest}"),
        StatusKind::Blocked => match i.blocked_reason {
            Some(BlockedReason::OlderThanInstalled) => {
                format!("{latest} is blocked: older than the installed build")
            }
            _ => format!("{latest} is blocked: it does not load on this client"),
        },
        StatusKind::Failed => format!(
            "failed: {}",
            i.error.as_ref().map_or("", |e| e.message.as_str())
        ),
        StatusKind::Current => "current".to_owned(),
        StatusKind::Static => "static (nothing upstream to check)".to_owned(),
        StatusKind::Unchecked => {
            let why = match i.unchecked_reason {
                Some(UncheckedReason::Offline) => "offline",
                Some(UncheckedReason::SourceDisabled) => "its source is off",
                Some(UncheckedReason::NoEligibleRelease) => "no release for this client",
                Some(UncheckedReason::NeverChecked) | None => "never checked",
            };
            format!("not checked ({why})")
        }
    }
}

fn plural(n: usize, word: &str) -> String {
    if n == 1 {
        word.to_owned()
    } else {
        format!("{word}s")
    }
}

/// Runs `outdated`.
pub fn run(ctx: &Context, args: &OutdatedArgs) -> Result<OutdatedOutcome> {
    let world = World::load(ctx)?;
    let verbose = ctx.output.style.verbose > 0;
    let mut warnings = world.warnings.clone();
    let mut notes = Vec::new();
    let targets: Vec<&DiscoveredTarget> = world.select_many(ctx)?;
    let records = world.records()?;
    let selected: Vec<(&DiscoveredTarget, &TargetRecord)> = targets
        .iter()
        .filter_map(|d| records.get(&d.target.id).map(|r| (*d, r)))
        .collect();

    let engine = Engine::new(ctx, &world, "outdated")?;
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

    // Each distinct ref and channel once; nothing for static installs or sources that
    // are off, and offline, nothing that was never remembered.
    let mut wanted: Vec<Wanted> = Vec::new();
    for (_, record) in &selected {
        for a in record.addons.values() {
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
    }
    // The cache reports each ref it asks of a source as it answers.
    let answers = cache.get(&wanted);
    let fetched_refs: BTreeSet<String> = answers
        .iter()
        .filter(|(_, f)| f.origin == Origin::Fetched)
        .map(|(w, _)| w.r#ref.to_string())
        .collect();
    let requests = count_requests(&answers);

    // A failed fetch: a remembered failure speaks for itself; a remembered release is
    // used, said, and makes the run partial; nothing remembered fails the item.
    let mut partial = false;
    let mut unanswered: BTreeMap<(Ref, Channel), ErrorBody> = BTreeMap::new();
    for (w, fetched) in &answers {
        let Err(error) = &fetched.set else { continue };
        match cache.entry(&w.r#ref, w.channel) {
            Some(entry) if entry.failure.is_some() => {}
            Some(entry) => {
                let mut warning = Warning::item(error);
                warning.message = format!(
                    "{}: {}; using its release metadata from {}",
                    w.r#ref, error.message, entry.fetched_at
                );
                warnings.push(warning);
                partial = true;
            }
            None => {
                unanswered.insert((w.r#ref.clone(), w.channel), ErrorBody::from(error.clone()));
            }
        }
    }

    let pairs: Vec<(&Target, &TargetRecord)> =
        selected.iter().map(|(d, r)| (&d.target, *r)).collect();
    let evaluate = |facts: &FactIndex| {
        statuses(
            &pairs,
            &|r, channel| cache.entry(r, channel),
            facts,
            &enabled,
            &|target, record| installed_primary_interface(target, record, &world.table),
            &|r, c, family, newest| cache.remembered_look_back(r, c, family, newest),
        )
    };
    let mut facts = cache.facts().clone();
    let mut rows = evaluate(&facts);
    let mut learned: Vec<CandidateFact> = Vec::new();
    if args.verify {
        // The planner refuses an older build than an adopted one, as `update` does.
        let floor = |target_id: &str, addon: &str| {
            let d = targets.iter().find(|d| d.target.id == target_id)?;
            let record = records
                .get(target_id)?
                .addons
                .get(&TargetRecord::addon_key(addon))?;
            update_floor(&d.target, record, &world.table)
        };
        let verified = verify(&engine, &cache, &targets, &rows, &floor);
        partial |= verified.partial;
        warnings.extend(verified.warnings);
        notes.extend(verified.notes);
        learned = verified.facts;
        facts.extend(&learned);
        rows = evaluate(&facts);
    }
    let checked_at = wanted
        .iter()
        .filter_map(|w| cache.entry(&w.r#ref, w.channel))
        .map(|e| e.fetched_at)
        .min();
    // `--changelog`: the release sets of the refs with updates, before the memo write.
    let sets: BTreeMap<(Ref, Channel), ReleaseSet> = if args.changelog {
        rows.iter()
            .filter(|row| row.status.kind() == StatusKind::UpdateAvailable)
            .filter_map(|row| {
                let record = records
                    .get(&row.target)?
                    .addons
                    .get(&TargetRecord::addon_key(&row.addon))?;
                let set = cache.entry(&record.r#ref, record.channel)?.result().ok()?;
                Some(((record.r#ref.clone(), record.channel), set))
            })
            .collect()
    } else {
        BTreeMap::new()
    };
    // One memo write per command, even when nothing else succeeded.
    let remembered = cache.remember(&learned);
    warnings.extend(remembered.warnings);
    notes.extend(remembered.notes);

    let mut summary = Summary::default();
    let mut listed: BTreeMap<String, Vec<OutdatedItem>> = BTreeMap::new();
    for row in rows {
        let Some(record) = records
            .get(&row.target)
            .and_then(|r| r.addons.get(&TargetRecord::addon_key(&row.addon)))
        else {
            continue;
        };
        let status = settle(row.status, record, ctx.offline, &unanswered);
        summary.count(status.kind());
        if let UpdateStatus::Failed { error } = &status {
            partial = true;
            warnings.push(failure_warning(&row.target, &record.name, error));
        }
        if verbose || worth_listing(status.kind()) {
            listed
                .entry(row.target)
                .or_default()
                .push(item(record, status));
        }
    }
    if args.changelog {
        let asked = attach_changelogs(&engine, &records, &sets, &mut listed, &mut notes);
        // A ref whose metadata came from the memo but whose changelog was asked is a
        // fetch too; a ref fetched now was reported already.
        let more: BTreeSet<String> = asked.difference(&fetched_refs).cloned().collect();
        report_fetches(ctx, &more);
    }

    let mut out_targets = Vec::new();
    let mut headings = Vec::new();
    for d in &targets {
        let items = listed.remove(d.target.id.as_str()).unwrap_or_default();
        let t = &d.target;
        let modes = world.table.mode_names(t.product.as_deref());
        let v = t.version;
        headings.push(format!(
            "{} ({}.{}.{}, {})",
            t.id,
            v.major,
            v.minor,
            v.patch,
            modes_or_family(&modes, t.family)
        ));
        out_targets.push(OutdatedTarget {
            target: TargetRef {
                id: t.id.clone(),
                key: t.key,
                mode: None,
            },
            items,
        });
    }
    let mut commands = hints(&out_targets);
    if let Some(selector) = ctx.target.as_deref() {
        super::status::scope_update(&mut commands, selector);
    }
    commands.extend(super::status::adopt_hints(&world, &targets, &records));
    Ok(OutdatedOutcome {
        data: OutdatedData {
            checked_at,
            requests,
            targets: out_targets,
            summary,
        },
        warnings: dedup(warnings),
        commands,
        partial,
        check: args.check,
        headings,
        age: checked_at.map(age),
        notes,
    })
}

/// An installed release: its `release_id`, version and install time.
type InstalledRelease = (String, String, jiff::Timestamp);

/// `--changelog`: each `update_available` item's release notes since its installed
/// release. One changelog per distinct ref and channel, from the oldest installed release
/// among its items (B4's order); each item keeps the entries newer than its own (an
/// identified adoption without a release, the latest's alone). Other statuses get
/// nothing. What failed joins `notes` (for `-v`); returns the refs whose source was
/// asked, for the fetch progress.
fn attach_changelogs(
    engine: &Engine,
    records: &BTreeMap<String, TargetRecord>,
    sets: &BTreeMap<(Ref, Channel), ReleaseSet>,
    listed: &mut BTreeMap<String, Vec<OutdatedItem>>,
    notes: &mut Vec<String>,
) -> BTreeSet<String> {
    let updates = |i: &&mut OutdatedItem| i.status == StatusKind::UpdateAvailable;
    // Per ref and channel: the newest release, and every installed release with one.
    let mut groups: BTreeMap<(Ref, Channel), (String, Vec<InstalledRelease>)> = BTreeMap::new();
    for (target, items) in listed.iter_mut() {
        for i in items.iter_mut().filter(updates) {
            let Some(latest) = &i.latest else { continue };
            let group = groups
                .entry((i.r#ref.clone(), i.channel))
                .or_insert_with(|| (latest.release.release_id.clone(), Vec::new()));
            let installed_at = records
                .get(target)
                .and_then(|r| r.addons.get(&TargetRecord::addon_key(&i.name)))
                .map_or(jiff::Timestamp::UNIX_EPOCH, |a| a.installed_at);
            if let Some(release_id) = &i.installed.release_id {
                group.1.push((
                    release_id.clone(),
                    i.installed.version.clone(),
                    installed_at,
                ));
            }
        }
    }
    let mut asked = BTreeSet::new();
    let mut found: BTreeMap<(Ref, Channel), Notes> = BTreeMap::new();
    for (key, (latest_id, installed)) in &groups {
        let release = sets.get(key).and_then(|set| {
            set.releases
                .iter()
                .find(|r| r.release_id == *latest_id)
                .map(|r| (set, r))
        });
        let found_notes = match release {
            Some((set, release)) => {
                let installed: Vec<ChangelogInstalled<'_>> = installed
                    .iter()
                    .map(|(release_id, version, installed_at)| ChangelogInstalled {
                        release_id,
                        version,
                        installed_at: *installed_at,
                    })
                    .collect();
                let since = oldest_since(&installed, set);
                changelog_for(
                    engine.registry(),
                    set,
                    release,
                    since.as_deref(),
                    engine.http(),
                )
            }
            None => Notes {
                changelog: None,
                unavailable: Some(ChangelogUnavailable::NoReleases),
                diagnostic: None,
                asked: false,
            },
        };
        if found_notes.asked {
            asked.insert(key.0.to_string());
        }
        notes.extend(found_notes.diagnostic.clone());
        found.insert(key.clone(), found_notes);
    }
    for items in listed.values_mut() {
        for i in items.iter_mut().filter(updates) {
            let Some(notes) = found.get(&(i.r#ref.clone(), i.channel)) else {
                continue;
            };
            let Some(latest) = i.latest.as_mut() else {
                continue;
            };
            match &notes.changelog {
                Some(changelog) => {
                    let mine = match &i.installed.release_id {
                        Some(release_id) => newer_than(changelog, Some(identity(release_id))),
                        // Nothing to start from: the latest release's notes alone.
                        None => Changelog {
                            entries: changelog.entries.iter().take(1).cloned().collect(),
                            truncated: false,
                            ..changelog.clone()
                        },
                    };
                    latest.changelog = Some(Some(mine));
                }
                None => {
                    latest.changelog = Some(None);
                    latest.changelog_unavailable = notes.unavailable;
                }
            }
        }
    }
    asked
}

/// One more `fetch` progress event per ref in `refs`, the phase grown to count them.
fn report_fetches(ctx: &Context, refs: &BTreeSet<String>) {
    ctx.progress.grow(Phase::Fetch, refs.len() as u64);
    for r in refs {
        ctx.progress.event(Phase::Fetch, refs.len() as u64, r);
    }
}

fn count_requests(answers: &BTreeMap<Wanted, Fetched>) -> Requests {
    let mut requests = Requests::default();
    for fetched in answers.values() {
        match fetched.origin {
            Origin::Fetched => requests.fetched += 1,
            Origin::Probed => requests.probed += 1,
            Origin::Memo | Origin::StaleFallback => requests.reused += 1,
        }
    }
    requests
}

/// The evaluator's status, with what only this command knows: offline, an entry never
/// remembered is `offline`; a fetch that failed with nothing remembered is `failed`.
fn settle(
    status: UpdateStatus,
    record: &crate::model::AddonRecord,
    offline: bool,
    unanswered: &BTreeMap<(Ref, Channel), ErrorBody>,
) -> UpdateStatus {
    let never = UpdateStatus::Unchecked {
        reason: UncheckedReason::NeverChecked,
    };
    if status != never {
        return status;
    }
    if let Some(error) = unanswered.get(&(record.r#ref.clone(), record.channel)) {
        return UpdateStatus::Failed {
            error: error.clone(),
        };
    }
    if offline {
        return UpdateStatus::Unchecked {
            reason: UncheckedReason::Offline,
        };
    }
    status
}

/// A failed item, mirrored as a warning: its error's code and fix.
fn failure_warning(target: &str, name: &str, error: &ErrorBody) -> Warning {
    body_warning(error, format!("{target}: {name}: {}", error.message))
}

/// A warning with `error`'s code and fix, saying `message`.
fn body_warning(error: &ErrorBody, message: String) -> Warning {
    let fix = match &error.fix_argv {
        Some(argv) => Suggestion::command(argv.clone()),
        None => Suggestion::text(error.fix.clone()),
    };
    Warning::new(WarningCode::Item(error.code), message).with_fix(fix)
}

/// What the default listing shows: what needs a decision.
const fn worth_listing(kind: StatusKind) -> bool {
    matches!(
        kind,
        StatusKind::UpdateAvailable
            | StatusKind::Pinned
            | StatusKind::Blocked
            | StatusKind::Ahead
            | StatusKind::Failed
    )
}

fn item(record: &crate::model::AddonRecord, status: UpdateStatus) -> OutdatedItem {
    let latest = status.latest().cloned().map(|release| LatestRelease {
        release,
        changelog: None,
        changelog_unavailable: None,
    });
    let kind = status.kind();
    let (confidence, blocked_reason, unchecked_reason, error) = match status {
        UpdateStatus::UpdateAvailable { confidence, .. } => (Some(confidence), None, None, None),
        UpdateStatus::Blocked { reason, .. } => (None, Some(reason), None, None),
        UpdateStatus::Unchecked { reason } => (None, None, Some(reason), None),
        UpdateStatus::Failed { error } => (None, None, None, Some(error)),
        UpdateStatus::Current { .. }
        | UpdateStatus::Pinned { .. }
        | UpdateStatus::Ahead { .. }
        | UpdateStatus::Static => (None, None, None, None),
    };
    OutdatedItem {
        name: record.name.clone(),
        r#ref: record.r#ref.clone(),
        channel: record.channel,
        pinned: record.pinned,
        installed: Installed {
            version: record.version.clone(),
            release_id: record.release_id.clone(),
        },
        latest,
        status: kind,
        confidence,
        blocked_reason,
        unchecked_reason,
        error,
    }
}

/// How to take the updates: `omaddons update`, or `omaddons update -t <id>` when only
/// one target has updates.
fn hints(targets: &[OutdatedTarget]) -> Vec<CommandHint> {
    let with_updates: Vec<&str> = targets
        .iter()
        .filter(|t| {
            t.items
                .iter()
                .any(|i| i.status == StatusKind::UpdateAvailable)
        })
        .map(|t| t.target.id.as_str())
        .collect();
    let argv: Vec<String> = match with_updates.as_slice() {
        [] => return Vec::new(),
        [only] => ["omaddons", "update", "-t", only]
            .map(str::to_owned)
            .to_vec(),
        _ => ["omaddons", "update"].map(str::to_owned).to_vec(),
    };
    vec![CommandHint::new(Action::Update, argv)]
}

fn dedup(warnings: Vec<Warning>) -> Vec<Warning> {
    let mut unique: Vec<Warning> = Vec::new();
    for w in warnings {
        if !unique.contains(&w) {
            unique.push(w);
        }
    }
    unique
}

/// How long ago `at` was, in words.
fn age(at: Timestamp) -> String {
    let minutes = Timestamp::now().duration_since(at).as_secs() / 60;
    match minutes {
        ..1 => "just now".to_owned(),
        1..60 => format!("{minutes} min ago"),
        60..2880 => format!("{} h ago", minutes / 60),
        _ => format!("{} days ago", minutes / 1440),
    }
}

/// What `--verify` learned.
#[derive(Debug, Default)]
struct Verified {
    facts: Vec<CandidateFact>,
    warnings: Vec<Warning>,
    notes: Vec<String>,
    partial: bool,
}

/// Downloads and verifies each update not yet verified, as installing it would: one dry
/// run over every target with such updates, sharing the command's release metadata. A
/// download that fails is a warning and makes the run partial; other refusals (the
/// candidate does not load, ownership) are `-v` notes, and their facts speak.
fn verify(
    engine: &Engine,
    cache: &ReleaseCache<'_>,
    targets: &[&DiscoveredTarget],
    rows: &[StatusRow],
    floor: &dyn Fn(&str, &str) -> Option<u32>,
) -> Verified {
    let mut by_target: BTreeMap<&str, Vec<RequestedChange>> = BTreeMap::new();
    for row in rows {
        if let UpdateStatus::UpdateAvailable { confidence, .. } = row.status
            && confidence != Confidence::Verified
        {
            by_target
                .entry(row.target.as_str())
                .or_default()
                .push(RequestedChange::Update {
                    addon: row.addon.clone(),
                    installed_primary_interface: floor(&row.target, &row.addon),
                });
        }
    }
    let req = TxRequest {
        targets: targets
            .iter()
            .filter_map(|d| {
                let changes = by_target.remove(d.target.id.as_str())?;
                Some(TxTarget {
                    discovered: (*d).clone(),
                    changes,
                    default_kind: TxKind::Update,
                    dependency_locks: BTreeMap::new(),
                })
            })
            .collect(),
        force: false,
        // Verifying changes nothing, so a running client does not stop it.
        while_running: true,
        dry_run: true,
        reason: None,
        kind: None,
        // A dry run commits nothing, so its run id is never recorded.
        run: RunId::new(),
        // Verifying the updates themselves: their dependencies download at update time.
        no_deps: true,
        witness: None,
    };
    let mut verified = Verified::default();
    if req.targets.is_empty() {
        return verified;
    }
    match engine.run(&req, Some(cache)) {
        Ok(done) => {
            verified.facts = done.plan.facts.clone();
            // An update verified from an unchecked download says so.
            verified.warnings.extend(
                done.plan
                    .targets
                    .iter()
                    .flat_map(|t| &t.warnings)
                    .filter(|w| matches!(w.code, WarningCode::DownloadUnverified))
                    .cloned(),
            );
            for f in &done.failures {
                let what = f.addon.as_deref().unwrap_or("verification");
                if f.error.retryable {
                    let message = format!("{}: {what}: {}", f.target, f.error.message);
                    verified.warnings.push(body_warning(&f.error, message));
                    verified.partial = true;
                } else {
                    verified
                        .notes
                        .push(format!("verify: {}: {what}: {}", f.target, f.error.message));
                }
            }
        }
        Err(error) => {
            verified.warnings.push(Warning::item(&error));
            verified.partial = true;
        }
    }
    verified
}
