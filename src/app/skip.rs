//! `omaddons skip <addon>... [-t T]` and `omaddons unskip <addon>... [-t T]`: pass over
//! a release, and offer it again (regression guard, design 7.5.4 to 7.5.6).
//!
//! Both are records-only transactions of kind `skip`: no game file changes and no
//! request, so they run while the client runs, and like every change they are
//! journaled, snapshotted and undoable, as `pin` is. Without `-t` they act on every
//! client where an addon is installed, one transaction per client under one run.
//!
//! - Plain `skip` marks, by its exact key, the release the release memo says is waiting
//!   (`update_available`, or `pinned` with a newer release), whatever the memo's age:
//!   the status `outdated --offline` shows. Anything else is `unchanged`, and says why.
//! - `skip --release ID [--revision N]` marks exactly that release, and `skip --version V`
//!   makes a label mark, which holds every release whose version reads V.
//! - `unskip` clears every mark of the addons, those holding one release (`--release`),
//!   those labelled V (`--version`), or those one transaction made (`--tx`).
//!
//! A preview binds what it marks or clears: the planned change's `PolicyDelta` is in the
//! token, so a stored `skip` plan whose waiting release moved in the memo before `apply`
//! is `plan_stale`, never a mark on a release the preview did not show.

use std::collections::BTreeMap;
use std::io::Write;

use super::adopt::hints::{adopt_fixes, adopting};
use super::approval::{Approval, ApprovalFlags};
use super::engine::{Engine, recover_waiting};
use super::intent::{self, Preview, Storable, StorableArgs, TargetScope};
use super::outcome::{
    ItemDeps, ItemError, ItemRequest, ItemResult, ItemStatus, TargetRef, TargetsData,
    TargetsOutcome, item_detail, item_name,
};
use super::precondition::{self, Idle, PlanBinding, PlanExtras};
use super::remembered::Remembered;
use super::world::{World, names_addon};
use crate::cli::Context;
use crate::contract::actions::Action;
use crate::discover::DiscoveredTarget;
use crate::engine::edit::{RecordEdit, UnskipWhich};
use crate::engine::plan::RequestedChange;
use crate::engine::{TxRequest, TxTarget};
use crate::error::{CommandHint, Error, ErrorCode, Result, Suggestion, Warning};
use crate::model::{
    AddonRecord, ReleaseKey, RunId, SkipIdentity, SkippedNote, TxId, TxKind, skip_label,
};
use crate::output::{Outcome, Style};
use crate::resolve::updates::{UncheckedReason, UpdateStatus, skip_covering};
use crate::state::records::TargetRecord;

/// `skip`'s own arguments.
#[derive(Debug, Clone, Default, clap::Args)]
pub struct SkipArgs {
    /// The installed addons: each by its name, or a catalog name or alias
    #[arg(value_name = "ADDON", required = true, num_args = 1..)]
    pub addons: Vec<String>,
    /// Pass over exactly this release, by its id from outdated or list -v
    ///
    /// gh:Owner/Repo@v6.2.5, whatever the release memo offers now. Takes one addon.
    #[arg(long, value_name = "RELEASE-ID", conflicts_with = "version")]
    pub release: Option<String>,
    /// With --release: the release's source revision (WowAce)
    ///
    /// Without it, a source that numbers revisions takes the release memo's revision of
    /// that release, else the mark holds every revision of the tag.
    #[arg(long, value_name = "N", requires = "release")]
    pub revision: Option<u32>,
    /// Pass over every release whose version reads this (a label mark)
    ///
    /// For a release announced but not seen yet. Takes one addon.
    #[arg(long, value_name = "VERSION")]
    pub version: Option<String>,
}

/// `unskip`'s own arguments.
#[derive(Debug, Clone, Default, clap::Args)]
pub struct UnskipArgs {
    /// The installed addons: each by its name, or a catalog name or alias
    ///
    /// At least one, except with --tx: then every addon that transaction marked.
    #[arg(value_name = "ADDON", num_args = 0..)]
    pub addons: Vec<String>,
    /// Offer exactly this release again: clear the marks that hold it
    ///
    /// Its exact mark and a label mark reading its version. Takes one addon.
    #[arg(long, value_name = "RELEASE-ID", conflicts_with_all = ["version", "tx"])]
    pub release: Option<String>,
    /// With --release: the release's source revision (WowAce)
    ///
    /// Without it, the marks on every revision of the tag are cleared.
    #[arg(long, value_name = "N", requires = "release")]
    pub revision: Option<u32>,
    /// Clear the marks whose version reads this
    ///
    /// Exact marks and label marks alike, ignoring one leading v. Takes one addon.
    #[arg(long, value_name = "VERSION", conflicts_with = "tx")]
    pub version: Option<String>,
    /// Clear exactly the marks this transaction made
    ///
    /// Its full id, as history and the result of a skip or an undo give it.
    #[arg(long, value_name = "TX")]
    pub tx: Option<String>,
}

impl UnskipArgs {
    /// `--tx` as a transaction id; `usage` for one that is not a full id.
    fn tx_id(&self) -> Result<Option<TxId>> {
        self.tx
            .as_deref()
            .map(|tx| {
                tx.parse::<TxId>().map_err(|_| {
                    Error::new(
                        ErrorCode::Usage,
                        format!("--tx {tx} is not a transaction id: give its full id, as history shows it"),
                        Suggestion::command(["omaddons", "history"]),
                    )
                })
            })
            .transpose()
    }
}

/// What the command does.
#[derive(Debug, Clone, Copy)]
enum Mode<'a> {
    Skip(&'a SkipArgs),
    Unskip(&'a UnskipArgs),
}

impl<'a> Mode<'a> {
    const fn command(self) -> &'static str {
        match self {
            Self::Skip(_) => "skip",
            Self::Unskip(_) => "unskip",
        }
    }

    /// What a changed item is called.
    const fn status(self) -> ItemStatus {
        match self {
            Self::Skip(_) => ItemStatus::SkipMarked,
            Self::Unskip(_) => ItemStatus::SkipCleared,
        }
    }

    fn addons(self) -> &'a [String] {
        match self {
            Self::Skip(a) => &a.addons,
            Self::Unskip(a) => &a.addons,
        }
    }

    /// `--release` or `--version`: forms about one release (or label) of one addon.
    fn names_one_release(self) -> Option<&'static str> {
        let (release, version) = match self {
            Self::Skip(a) => (a.release.is_some(), a.version.is_some()),
            Self::Unskip(a) => (a.release.is_some(), a.version.is_some()),
        };
        if release {
            Some("--release")
        } else if version {
            Some("--version")
        } else {
            None
        }
    }
}

/// `skip`'s and `unskip`'s outcome: the shared multi-target outcome, rendered in their
/// own words.
#[derive(Debug)]
pub struct SkipOutcome {
    inner: TargetsOutcome,
    unskip: bool,
    /// Why an addon was left unchanged, by (client id, addon name), for the human lines.
    reasons: BTreeMap<(String, String), String>,
    /// Each committed transaction's global lock hold time, for `-v` log lines.
    pub locks: Vec<(TxId, u64)>,
    /// For `-v` log lines.
    pub notes: Vec<String>,
}

impl Storable for SkipOutcome {
    fn take_preview(&mut self) -> Option<Preview> {
        self.inner.take_preview()
    }

    fn record_plan(
        &mut self,
        plan_id: crate::model::PlanId,
        expires_at: jiff::Timestamp,
        hint: CommandHint,
    ) {
        self.inner.record_plan(plan_id, expires_at, hint);
    }
}

impl Outcome for SkipOutcome {
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

    fn render_human(&self, w: &mut dyn Write, style: &Style) -> std::io::Result<()> {
        let data = &self.inner.data;
        if data.targets.is_empty() {
            writeln!(w, "Nothing to {}.", self.inner.command)?;
        }
        for t in &data.targets {
            render_target(w, t, data.dry_run, self.unskip, &self.reasons)?;
        }
        let committed: Vec<TxId> = data.targets.iter().filter_map(|t| t.tx).collect();
        match committed.as_slice() {
            [] => {}
            [tx] => writeln!(w, "undo: omaddons undo {tx}")?,
            _ => writeln!(w, "Undo all of it with: omaddons undo")?,
        }
        for note in &self.inner.notes {
            writeln!(w, "{note}")?;
        }
        intent::render_plan_line(w, data.plan_id.as_ref(), data.plan_expires_at.as_ref())?;
        match data.precondition.as_deref() {
            Some(token) if style.detail() => {
                intent::render_bind_line(w, token, data.plan_id.is_some())
            }
            _ => Ok(()),
        }
    }
}

/// One client's lines: each mark made or cleared in words (design 5.4), each addon left
/// alone with why, each failure with its fix.
fn render_target(
    w: &mut dyn Write,
    t: &super::outcome::TargetResult,
    dry_run: bool,
    unskip: bool,
    reasons: &BTreeMap<(String, String), String>,
) -> std::io::Result<()> {
    let id = &t.target.id;
    if let Some(error) = &t.error {
        return writeln!(w, "{id}: failed: {}: {}", error.code.wire(), error.message);
    }
    let tx =
        t.tx.map(|tx| format!(" (tx {})", super::history::short_tx(&tx)))
            .unwrap_or_default();
    let notes = if unskip {
        t.skips_cleared.as_deref()
    } else {
        t.skips_marked.as_deref()
    }
    .unwrap_or_default();
    for item in &t.items {
        let name = item_name(item);
        match item.status {
            ItemStatus::SkipMarked | ItemStatus::SkipCleared => {
                let installed = item.to_version.as_deref().unwrap_or("?");
                let key = TargetRecord::addon_key(name);
                let mine = notes
                    .iter()
                    .filter(|n| TargetRecord::addon_key(&n.addon) == key);
                for note in mine {
                    writeln!(
                        w,
                        "{}{tx}",
                        mark_line(id, name, note, installed, dry_run, unskip)
                    )?;
                }
            }
            // Said below, with why, whether `-v` listed the item or not.
            ItemStatus::Unchanged => {}
            _ => {
                writeln!(w, "{id}: {name}  {}", item_detail(item, dry_run))?;
                if let Some(error) = &item.error {
                    writeln!(w, "  fix: {}", error.fix)?;
                }
            }
        }
    }
    for ((client, name), why) in reasons {
        if client == id {
            writeln!(w, "{id}: {name}: {why}")?;
        }
    }
    Ok(())
}

/// `retail: Plater v662 is skipped (installed: v661): update resumes at the next release`,
/// a label mark said as one, an exact mark without a revision on a source that numbers
/// them said to hold every revision; `retail: Plater v662 is offered again`.
fn mark_line(
    id: &str,
    name: &str,
    note: &SkippedNote,
    installed: &str,
    dry_run: bool,
    unskip: bool,
) -> String {
    let version = &note.version;
    let what = match &note.release {
        None => format!("every release labelled {version} (a label mark)"),
        Some(key) if key.revision.is_none() && numbers_revisions(key) => {
            format!("{version} (every revision of its tag)")
        }
        Some(key) => match key.revision {
            Some(revision) => format!("{version} (revision {revision})"),
            None => version.clone(),
        },
    };
    match (unskip, dry_run) {
        (false, true) => format!("{id}: would skip {name} {what} (installed: {installed})"),
        (false, false) => format!(
            "{id}: {name} {what} is skipped (installed: {installed}): update resumes at the next release"
        ),
        (true, true) => format!("{id}: would offer {name} {what} again"),
        (true, false) => format!("{id}: {name} {what} is offered again"),
    }
}

/// Whether the release's source numbers revisions of a tag (WowAce).
fn numbers_revisions(key: &ReleaseKey) -> bool {
    key.source.starts_with("wowace:")
}

/// Runs `skip`.
pub fn skip(ctx: &Context, args: &SkipArgs) -> Result<SkipOutcome> {
    run(ctx, Mode::Skip(args))
}

/// Runs `unskip`.
pub fn unskip(ctx: &Context, args: &UnskipArgs) -> Result<SkipOutcome> {
    run(ctx, Mode::Unskip(args))
}

/// What the command decided for one addon on one client.
enum Decision {
    Edit(RecordEdit),
    /// Left as it is, and why (for the human line and `-v`).
    Unchanged(String),
}

fn run(ctx: &Context, mode: Mode<'_>) -> Result<SkipOutcome> {
    let command = mode.command();
    if let Mode::Unskip(UnskipArgs {
        addons, tx: None, ..
    }) = mode
        && addons.is_empty()
    {
        return Err(Error::new(
            ErrorCode::Usage,
            "unskip names the addons whose marks to clear, or --tx the transaction that made them",
            Suggestion::command(["omaddons", "help", "unskip"]),
        ));
    }
    if let Some(flag) = mode.names_one_release()
        && mode.addons().len() != 1
    {
        return Err(Error::new(
            ErrorCode::Usage,
            format!("{flag} names one release of one addon; name exactly one addon"),
            Suggestion::command(["omaddons", "help", command]),
        ));
    }
    let release_key = match mode {
        Mode::Skip(SkipArgs {
            release: Some(id),
            revision,
            ..
        })
        | Mode::Unskip(UnskipArgs {
            release: Some(id),
            revision,
            ..
        }) => Some(parse_release(id, *revision)?),
        _ => None,
    };
    let unskip_tx = match mode {
        Mode::Unskip(args) => args.tx_id()?,
        Mode::Skip(_) => None,
    };
    let world = World::load(ctx)?;
    let verbose = ctx.output.style.detail();
    let mut warnings = world.warnings.clone();
    let mut notes = Vec::new();
    let (catalog, catalog_warnings) = world.catalog(ctx);
    warnings.extend(catalog_warnings);
    let mut targets: Vec<&DiscoveredTarget> = world.select_many(ctx)?;
    targets.sort_by(|a, b| a.target.id.cmp(&b.target.id));
    let records = world.records()?;
    let remembered = Remembered::load(&world);
    notes.extend(remembered.note.clone());

    // Which addons on which clients: each named one where it is installed, or, for
    // `unskip --tx` alone, every addon holding a mark that transaction made.
    let mut found: Vec<(&DiscoveredTarget, &AddonRecord, String)> = Vec::new();
    let mut unknown: Vec<String> = Vec::new();
    match (unskip_tx, mode.addons()) {
        (Some(tx), []) => {
            for d in &targets {
                let Some(record) = records.get(&d.target.id) else {
                    continue;
                };
                for a in record.addons.values() {
                    if a.skipped_releases.iter().any(|m| m.tx == tx) {
                        found.push((*d, a, a.name.clone()));
                    }
                }
            }
        }
        (_, names) => {
            for name in names {
                let before = found.len();
                for d in &targets {
                    let Some(record) = records.get(&d.target.id) else {
                        continue;
                    };
                    if let Some(a) = record
                        .addons
                        .values()
                        .find(|a| names_addon(name, a, &catalog))
                    {
                        found.push((*d, a, name.clone()));
                    }
                }
                if found.len() == before {
                    unknown.push(name.clone());
                }
            }
        }
    }
    let not_installed = |name: &str| {
        let checked: Vec<&str> = targets.iter().map(|d| d.target.id.as_str()).collect();
        adopting(
            Error::new(
                ErrorCode::NotManaged,
                format!("{name} is not installed on {}", checked.join(", ")),
                Suggestion::command(["omaddons", "install", name]),
            ),
            adopt_fixes(&world, &records, &targets, name),
        )
    };
    if found.is_empty()
        && let Some(name) = unknown.first()
    {
        return Err(not_installed(name));
    }
    // `skip --release` names a release of the addon's own source, or it would never hold.
    if let (Mode::Skip(_), Some(key)) = (mode, &release_key)
        && let Some((_, a, _)) = found
            .iter()
            .find(|(_, a, _)| a.r#ref.identity() != key.source)
    {
        let id = mode_release_id(mode).unwrap_or_default();
        return Err(Error::new(
            ErrorCode::Usage,
            format!(
                "{id} is a release of {}; {} comes from {}",
                key.source, a.name, a.r#ref
            ),
            Suggestion::command(["omaddons", "list", "-v"]),
        ));
    }

    let mut changes: BTreeMap<String, Vec<RequestedChange>> = BTreeMap::new();
    let mut requested = Vec::new();
    let mut unchanged = Vec::new();
    let mut reasons = BTreeMap::new();
    for (d, a, request) in &found {
        let decision = decide(
            mode,
            d,
            a,
            &records,
            &remembered,
            &world,
            release_key.as_ref(),
            unskip_tx,
        );
        match decision {
            Decision::Edit(edit) => {
                changes
                    .entry(d.target.id.clone())
                    .or_default()
                    .push(RequestedChange::Edit {
                        addon: a.name.clone(),
                        edit,
                    });
                requested.push(ItemRequest {
                    request: request.clone(),
                    target: d.target.id.clone(),
                    addon: Some(a.name.clone()),
                    r#ref: Some(a.r#ref.clone()),
                    version: Some(a.version.clone()),
                });
            }
            Decision::Unchanged(why) => {
                notes.push(format!("{}: {} unchanged: {why}", d.target.id, a.name));
                reasons.insert((d.target.id.clone(), a.name.clone()), why);
                unchanged.push((
                    target_ref(d),
                    ItemResult {
                        request: request.clone(),
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
                ));
            }
        }
    }

    let run = RunId::new();
    let mut data = TargetsData::assemble(&[], &[], &[], ctx.dry_run, run, &[]);
    let mut locks = Vec::new();
    let mut preview = None;
    let flags = ApprovalFlags {
        while_running: ctx.while_running,
        ..ApprovalFlags::default()
    };
    let token;
    if !changes.is_empty() {
        let engine = Engine::new(ctx, &world, command)?;
        let req = TxRequest {
            targets: targets
                .iter()
                .filter_map(|d| {
                    Some(TxTarget {
                        discovered: (*d).clone(),
                        changes: changes.remove(&d.target.id)?,
                        default_kind: TxKind::Skip,
                        dependency_locks: BTreeMap::new(),
                    })
                })
                .collect(),
            force: false,
            while_running: ctx.while_running,
            dry_run: ctx.dry_run,
            reason: ctx.reason.clone(),
            kind: Some(TxKind::Skip),
            run,
            no_deps: false,
            witness: precondition::witnessing(ctx),
        };
        let extras = PlanExtras::default();
        let binding = PlanBinding::of_run(ctx, command, extras.clone());
        // Records only: no release is fetched, so there is no cache to ask.
        let done = engine.run_bound(
            &req,
            None,
            binding.as_ref().map(|b| b as &dyn crate::engine::Binding),
        )?;
        let plan_precondition = ctx
            .dry_run
            .then(|| precondition::plan_precondition(command, &done.plan, &extras));
        token = plan_precondition.as_ref().map(|p| p.token());
        data = TargetsData::build(&done, run, &requested);
        if ctx.dry_run {
            data = data.with_approval(Some(Approval::of_plan(&done.plan, &records, &flags)?));
            if let Some(p) = plan_precondition.filter(|_| done.plan.has_ops()) {
                let selected: Vec<_> = targets.iter().map(|d| &d.target).collect();
                let scope = TargetScope::of(ctx.target.as_deref(), &selected);
                let args = match mode {
                    Mode::Skip(args) => StorableArgs::Skip { args, scope },
                    Mode::Unskip(args) => StorableArgs::Unskip { args, scope },
                };
                preview = intent::preview_plan(ctx, &args, p, &done.plan, &records)?;
            }
        }
        for o in &done.outcomes {
            warnings.extend(o.warnings.iter().cloned());
            notes.extend(o.notes.iter().cloned());
            if let (Some(tx), Some(ms)) = (o.tx, o.lock_held_ms) {
                locks.push((tx, ms));
            }
        }
        warnings.extend(done.warnings.iter().cloned());
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
            // The marks each client's change made or cleared, from what it committed
            // (under `--dry-run`, what it would).
            let mut made = Vec::new();
            let mut cleared = Vec::new();
            for o in done.outcomes.iter().filter(|o| o.target == id) {
                for c in &o.changes {
                    made.extend(
                        c.policy
                            .skips_added
                            .iter()
                            .map(|m| SkippedNote::of(&c.addon, m)),
                    );
                    cleared.extend(
                        c.policy
                            .skips_removed
                            .iter()
                            .map(|m| SkippedNote::of(&c.addon, m)),
                    );
                }
            }
            made.sort();
            cleared.sort();
            match mode {
                Mode::Skip(_) => t.skips_marked = Some(made),
                Mode::Unskip(_) => t.skips_cleared = Some(cleared),
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
        if ctx.dry_run {
            data = data.with_approval(Some(Approval::nothing(&flags)));
        }
    }
    for (target, item) in unchanged {
        data.push_item(target, item);
    }
    // Each name installed nowhere is a failed item on the first client, its own error
    // kept whole for a run that has nothing else.
    let mut unknown_errors = Vec::new();
    if let Some(first) = targets.first() {
        for name in &unknown {
            let error = not_installed(name);
            unknown_errors.push(error.clone());
            data.push_item(
                target_ref(first),
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
    // Every client a skip names keeps its (possibly empty) list, so the key is there.
    for t in &mut data.targets {
        match mode {
            Mode::Skip(_) => {
                t.skips_marked.get_or_insert_with(Vec::new);
            }
            Mode::Unskip(_) => {
                t.skips_cleared.get_or_insert_with(Vec::new);
            }
        }
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
        if let ([only], 1) = (unknown_errors.as_slice(), data.summary.failed) {
            return Err(only.clone());
        }
        return Err(error);
    }
    let hints = match mode {
        Mode::Unskip(_) => unskip_hints(&world, &remembered, &records, &found, &data),
        Mode::Skip(_) => Vec::new(),
    };
    data.precondition = token;
    let data = data.listing(verbose);
    let mut commands = data.undo_commands();
    // What a skip marked, offered again by exactly its transaction: never every mark.
    if let Mode::Skip(_) = mode {
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
            notes: hints,
            preview,
        },
        unskip: matches!(mode, Mode::Unskip(_)),
        reasons,
        locks,
        notes,
    })
}

/// The release id `--release` names, as typed.
fn mode_release_id(mode: Mode<'_>) -> Option<String> {
    match mode {
        Mode::Skip(a) => a.release.clone(),
        Mode::Unskip(a) => a.release.clone(),
    }
}

/// `--release ID` (with `--revision N`) as a key; `usage` for an id that names no
/// release (`<ref>@<tag>`).
fn parse_release(id: &str, revision: Option<u32>) -> Result<ReleaseKey> {
    ReleaseKey::parse(id, revision).ok_or_else(|| {
        Error::new(
            ErrorCode::Usage,
            format!(
                "{id} is not a release id: one is <ref>@<tag>, as outdated and list -v print it (gh:Owner/Repo@v6.2.5)"
            ),
            Suggestion::command(["omaddons", "list", "-v"]),
        )
    })
}

/// What `mode` does to the addon `a` on client `d`: `release_key` is `--release` (with
/// `--revision`) as typed, `unskip_tx` unskip's `--tx`.
#[expect(
    clippy::too_many_arguments,
    reason = "one decision per addon from what the run resolved once"
)]
fn decide(
    mode: Mode<'_>,
    d: &DiscoveredTarget,
    a: &AddonRecord,
    records: &BTreeMap<String, TargetRecord>,
    remembered: &Remembered,
    world: &World,
    release_key: Option<&ReleaseKey>,
    unskip_tx: Option<TxId>,
) -> Decision {
    let id = &d.target.id;
    match mode {
        Mode::Skip(args) => {
            if let Some(version) = &args.version {
                let label = SkipIdentity::Label(skip_label(version).to_owned());
                if a.skipped_releases.iter().any(|m| m.selects() == label) {
                    return Decision::Unchanged(format!(
                        "every release labelled {version} is skipped already"
                    ));
                }
                return Decision::Edit(RecordEdit::Skip {
                    release: None,
                    version: version.clone(),
                });
            }
            if let Some(key) = release_key {
                let id_typed = args.release.as_deref().unwrap_or_default();
                let known = remembered.release(&a.r#ref, a.channel, id_typed);
                // On a source that numbers revisions, the memo's revision of the release
                // when it holds it; otherwise the mark holds every revision of the tag.
                let key = match (args.revision, known.and_then(|r| r.revision)) {
                    (None, Some(revision)) => ReleaseKey {
                        revision: Some(revision),
                        ..key.clone()
                    },
                    _ => key.clone(),
                };
                let version = known.map_or_else(|| key.tag.clone(), |r| r.version.clone());
                return mark(a, key, version);
            }
            let status = records.get(id).and_then(|record| {
                remembered
                    .statuses(&d.target, record, &world.table)
                    .into_iter()
                    .find(|row| {
                        TargetRecord::addon_key(&row.addon) == TargetRecord::addon_key(&a.name)
                    })
                    .map(|row| row.status)
            });
            match status {
                Some(
                    UpdateStatus::UpdateAvailable { latest, .. }
                    | UpdateStatus::Pinned {
                        latest: Some(latest),
                    },
                ) => match ReleaseKey::parse(&latest.release_id, latest.revision) {
                    Some(key) => mark(a, key, latest.version),
                    None => Decision::Unchanged(format!(
                        "its waiting release {} names no release to mark",
                        latest.version
                    )),
                },
                Some(status) => Decision::Unchanged(nothing_waiting(&status, a, id)),
                None => Decision::Unchanged(format!(
                    "nothing is remembered about its releases: run `omaddons outdated -t {id}` first"
                )),
            }
        }
        Mode::Unskip(args) => {
            let which = if let Some(tx) = unskip_tx {
                UnskipWhich::Tx(tx)
            } else if let Some(key) = release_key {
                let id_typed = args.release.as_deref().unwrap_or_default();
                let version = remembered
                    .release(&a.r#ref, a.channel, id_typed)
                    .map_or_else(|| key.tag.clone(), |r| r.version.clone());
                UnskipWhich::Release {
                    key: key.clone(),
                    version,
                }
            } else if let Some(version) = &args.version {
                UnskipWhich::Label(version.clone())
            } else {
                UnskipWhich::All
            };
            if a.skipped_releases.iter().any(|m| which.clears(m)) {
                Decision::Edit(RecordEdit::Unskip(which))
            } else if a.skipped_releases.is_empty() {
                Decision::Unchanged("no release is skipped".to_owned())
            } else {
                Decision::Unchanged("no skip mark matches".to_owned())
            }
        }
    }
}

/// The mark of the release `key`, labelled `version`, unless a mark holds it already.
fn mark(a: &AddonRecord, key: ReleaseKey, version: String) -> Decision {
    if let Some(held) = skip_covering(a, Some(&key), &version) {
        return Decision::Unchanged(format!(
            "{version} is skipped already (marked {})",
            held.at.strftime("%Y-%m-%d")
        ));
    }
    Decision::Edit(RecordEdit::Skip {
        release: Some(key),
        version,
    })
}

/// Why plain `skip` found no release waiting, by the status `outdated` would show.
fn nothing_waiting(status: &UpdateStatus, a: &AddonRecord, id: &str) -> String {
    let version = &a.version;
    match status {
        UpdateStatus::Current { .. } => format!("{version} is the newest release: none is waiting"),
        UpdateStatus::Ahead { latest } => format!(
            "{version} is newer than its channel's newest release, {}: none is waiting",
            latest.version
        ),
        UpdateStatus::Blocked { latest, .. } => format!(
            "its newest release, {}, does not load here, and update leaves it already",
            latest.version
        ),
        UpdateStatus::Pinned { .. } => "pinned, and no newer release is known".to_owned(),
        UpdateStatus::Failed { error } => format!(
            "its source could not answer ({}): run `omaddons outdated -t {id}`",
            error.code.wire()
        ),
        UpdateStatus::Unchecked { reason } => match reason {
            UncheckedReason::NeverChecked | UncheckedReason::Offline => format!(
                "nothing is remembered about its releases: run `omaddons outdated -t {id}` first"
            ),
            UncheckedReason::SourceDisabled => "its source is turned off".to_owned(),
            UncheckedReason::NoEligibleRelease => {
                "no release on its channel has a build for this client".to_owned()
            }
        },
        UpdateStatus::Static => {
            "it comes from a local file, a folder or a URL: there is no release to skip".to_owned()
        }
        UpdateStatus::UpdateAvailable { latest, .. } => {
            format!("{} is waiting", latest.version)
        }
    }
}

/// The clients examined to decide that no record needs a change.
fn noop_targets<'a>(
    found: &[(&'a DiscoveredTarget, &AddonRecord, String)],
    records: &BTreeMap<String, TargetRecord>,
) -> Vec<Idle<'a>> {
    let mut idle: Vec<Idle<'a>> = Vec::new();
    for (d, _, _) in found {
        if idle.iter().any(|i| i.target.id == d.target.id) {
            continue;
        }
        idle.push(Idle {
            target: &d.target,
            generation: records.get(&d.target.id).map_or(0, |r| r.generation),
        });
    }
    idle
}

fn target_ref(d: &DiscoveredTarget) -> TargetRef {
    TargetRef {
        id: d.target.id.clone(),
        key: d.target.key,
        mode: None,
    }
}

/// What `update` would do with each addon whose marks were just cleared, from the
/// release memo and the shared evaluator: no request (as `unpin`'s last line).
fn unskip_hints(
    world: &World,
    remembered: &Remembered,
    records: &BTreeMap<String, TargetRecord>,
    found: &[(&DiscoveredTarget, &AddonRecord, String)],
    data: &TargetsData,
) -> Vec<String> {
    let mut hints = Vec::new();
    for (d, a, _) in found {
        let cleared = data.targets.iter().any(|t| {
            t.target.id == d.target.id
                && t.items.iter().any(|i| {
                    i.status == ItemStatus::SkipCleared
                        && i.name.as_deref().map(TargetRecord::addon_key)
                            == Some(TargetRecord::addon_key(&a.name))
                })
        });
        let Some(record) = records.get(&d.target.id).filter(|_| cleared) else {
            continue;
        };
        let (id, name) = (&d.target.id, &a.name);
        let row = remembered
            .statuses(&d.target, record, &world.table)
            .into_iter()
            .find(|row| TargetRecord::addon_key(&row.addon) == TargetRecord::addon_key(name));
        hints.push(match row.map(|row| row.status) {
            Some(UpdateStatus::UpdateAvailable { latest, .. }) => format!(
                "{id}: `omaddons update {name} -t {id}` installs {}",
                latest.version
            ),
            Some(UpdateStatus::Current { .. }) => format!("{id}: {name} is current"),
            _ => format!("{id}: `omaddons outdated -t {id}` checks {name} for updates"),
        });
    }
    hints
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(release: Option<(&str, Option<u32>)>, version: &str) -> SkippedNote {
        SkippedNote {
            addon: "Plater".into(),
            version: version.into(),
            release: release.and_then(|(id, rev)| ReleaseKey::parse(id, rev)),
        }
    }

    #[test]
    fn mark_lines_say_what_a_mark_holds() {
        // (case, note, dry run, unskip, the line)
        let cases = [
            (
                "an exact mark",
                note(Some(("gh:Tercioo/Plater-Nameplates@v662", None)), "v662"),
                false,
                false,
                "retail: Plater v662 is skipped (installed: v661): update resumes at the next release",
            ),
            (
                "its dry run",
                note(Some(("gh:Tercioo/Plater-Nameplates@v662", None)), "v662"),
                true,
                false,
                "retail: would skip Plater v662 (installed: v661)",
            ),
            (
                "a label mark says it is one",
                note(None, "v662"),
                false,
                false,
                "retail: Plater every release labelled v662 (a label mark) is skipped (installed: v661): update resumes at the next release",
            ),
            (
                "a WowAce mark without a revision holds every revision",
                note(Some(("wowace:plater@v662", None)), "v662"),
                false,
                false,
                "retail: Plater v662 (every revision of its tag) is skipped (installed: v661): update resumes at the next release",
            ),
            (
                "a WowAce mark with one names it",
                note(Some(("wowace:plater@v662", Some(1402))), "v662"),
                false,
                false,
                "retail: Plater v662 (revision 1402) is skipped (installed: v661): update resumes at the next release",
            ),
            (
                "an unskip",
                note(Some(("gh:Tercioo/Plater-Nameplates@v662", None)), "v662"),
                false,
                true,
                "retail: Plater v662 is offered again",
            ),
            (
                "its dry run",
                note(Some(("gh:Tercioo/Plater-Nameplates@v662", None)), "v662"),
                true,
                true,
                "retail: would offer Plater v662 again",
            ),
        ];
        for (case, note, dry_run, unskip, line) in cases {
            assert_eq!(
                mark_line("retail", "Plater", &note, "v661", dry_run, unskip),
                line,
                "{case}: note={note:?}"
            );
        }
    }

    #[test]
    fn every_status_but_a_waiting_release_says_why_nothing_is_marked() {
        use crate::resolve::updates::{BlockedReason, Confidence, ReleaseBrief};
        let latest = ReleaseBrief {
            version: "v3".into(),
            release_id: "gh:o/foo@v3".into(),
            published_at: None,
            channel: crate::model::Channel::Stable,
            revision: None,
            retagged: false,
            asset: None,
        };
        let a = AddonRecord {
            r#ref: "gh:o/foo".parse().unwrap(),
            name: "Foo".into(),
            version: "v2".into(),
            version_confidence: crate::model::VersionConfidence::Release,
            release_id: Some("gh:o/foo@v2".into()),
            channel: crate::model::Channel::Stable,
            pinned: false,
            reason: crate::model::AddonReason::Explicit,
            required_by: Vec::new(),
            folders: vec![crate::model::FolderName::new("Foo").unwrap()],
            state: crate::model::AddonState::Ok,
            installed_at: "2026-10-14T21:00:00Z".parse().unwrap(),
            tx_id: "01JB8Q2VA00000000000000000".parse().unwrap(),
            blob_sha256: None,
            revision: None,
            left_out: Vec::new(),
            any_flavor: false,
            skipped_releases: Vec::new(),
        };
        // (case, status, words the reason holds)
        let cases = [
            (
                "current",
                UpdateStatus::Current {
                    latest: latest.clone(),
                },
                "v2 is the newest release",
            ),
            (
                "ahead",
                UpdateStatus::Ahead {
                    latest: latest.clone(),
                },
                "newer than its channel's newest release, v3",
            ),
            (
                "blocked",
                UpdateStatus::Blocked {
                    latest: latest.clone(),
                    reason: BlockedReason::VerifiedNotLoading,
                },
                "v3, does not load here",
            ),
            (
                "pinned with nothing newer",
                UpdateStatus::Pinned { latest: None },
                "pinned, and no newer release is known",
            ),
            (
                "never checked",
                UpdateStatus::Unchecked {
                    reason: UncheckedReason::NeverChecked,
                },
                "run `omaddons outdated -t retail` first",
            ),
            (
                "source off",
                UpdateStatus::Unchecked {
                    reason: UncheckedReason::SourceDisabled,
                },
                "its source is turned off",
            ),
            (
                "no build",
                UpdateStatus::Unchecked {
                    reason: UncheckedReason::NoEligibleRelease,
                },
                "no release on its channel has a build",
            ),
            (
                "static",
                UpdateStatus::Static,
                "there is no release to skip",
            ),
            (
                "an update, said as waiting",
                UpdateStatus::UpdateAvailable {
                    latest,
                    confidence: Confidence::Likely,
                },
                "v3 is waiting",
            ),
        ];
        for (case, status, words) in cases {
            let why = nothing_waiting(&status, &a, "retail");
            assert!(why.contains(words), "{case}: {why:?} lacks {words:?}");
        }
    }

    #[test]
    fn a_release_id_is_a_ref_and_a_tag() {
        // (case, typed, revision, parses)
        let cases = [
            ("a GitHub release", "gh:o/foo@v2", None, true),
            (
                "a WowAce tag at a revision",
                "wowace:bug-grabber@v12.1.1",
                Some(403),
                true,
            ),
            ("no tag", "gh:o/foo", None, false),
            ("an empty tag", "gh:o/foo@", None, false),
            ("no ref", "v2", None, false),
        ];
        for (case, typed, revision, parses) in cases {
            let parsed = parse_release(typed, revision);
            assert_eq!(parsed.is_ok(), parses, "{case}: {typed} → {parsed:?}");
            if let Err(e) = parsed {
                assert_eq!(e.code, ErrorCode::Usage, "{case}");
            }
        }
    }
}
