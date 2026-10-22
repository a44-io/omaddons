//! `omaddons errors [-t T] [--since D] [--addon A] [--limit N] [--received]
//! [--raw-details] [--check]`: which addon most likely throws Lua errors, how often,
//! since when, and whether an update may fix it, from BugGrabber's SavedVariables on
//! every client. A Look command: it reads, and writes only the derived errors memo in
//! the cache (M3-D20).
//!
//! Per client, one observation is built first from every entry. The memo, the counts and
//! `--check` derive from it; only then do `--since`, `--addon` and `--limit` shape the
//! view, so a filtered read never makes the bar think an addon is clean (M3-D19,
//! M3-D22). Default and `-v` output hold only code-derived text; free text needs
//! `--raw-details` (M3-D21).

use std::io::Write;

use jiff::Timestamp;
use jiff::tz::TimeZone;
use schemars::JsonSchema;
use serde::Serialize;

use super::history::{local_time, parse_since};
use super::list::Latest;
use super::precondition::{DismissPart, check, of_dismiss, preview_argv};
use super::remembered::Remembered;
use super::world::World;
use crate::cli::Context;
use crate::discover::running::detect_running;
use crate::engine::journal::{no_saved_variables, recover_under};
use crate::error::{CommandHint, Error, ErrorCode, Result, Suggestion, Warning};
use crate::errors::ack::{self, Ack, ErrorsAck, db_key};
use crate::errors::attribute::{Confidence, Method};
use crate::errors::buggrabber::{DbShape, EntryType, Origin, TimeEvidence, TimeMeaning, Tri};
use crate::errors::locate::{Capture, Coverage};
use crate::errors::memo::{self, ErrorsMemo, target_memo};
use crate::errors::observe::{
    AddonErrors, Observation, Observe, Observed, ReadState, SourceKind, group, read_target,
    shown_time,
};
use crate::errors::sanitize::{MessageKind, raw_details, strict_frames, strict_message};
use crate::fsx::{FileStamp, GlobalLock, ScopeTable, WriteSession, Writer, ensure_dir_all};
use crate::inventory::{Depth, inventory_shared};
use crate::limits::{ERRORS_MAX_TOTAL_BYTES, ERRORS_SHOWN_PER_ADDON};
use crate::model::TxKind;
use crate::model::{Target, TargetKey};
use crate::output::{Outcome, Style};
use crate::privacy::WtfPathRedactor;
use crate::resolve::catalog::Catalog;
use crate::resolve::updates::{StatusKind, UpdateStatus};
use crate::state::history::{self, Dismissed, SettingEntry};
use crate::state::paths::ClientPaths;
use crate::state::records::{TargetRecord, load_record};

/// The default `--limit`: addons shown per client.
const DEFAULT_LIMIT: usize = 10;

/// `errors`' own flags.
#[derive(Debug, Clone, Default, clap::Args)]
pub struct ErrorsArgs {
    #[command(subcommand)]
    pub action: Option<ErrorsAction>,
    /// Only errors seen since then: 30m, 12h, 1d, 2w, or an RFC 3339 time
    #[arg(long, value_name = "WHEN")]
    pub since: Option<String>,
    /// Only one addon's errors (by addon or folder name)
    #[arg(long, value_name = "ADDON")]
    pub addon: Option<String>,
    /// Addons shown per client [default: 10]
    #[arg(long, value_name = "N")]
    pub limit: Option<usize>,
    /// Also list the errors other players sent through BugSack, apart
    #[arg(long)]
    pub received: bool,
    /// Also show messages, locals and code as written (they may hold names)
    #[arg(long)]
    pub raw_details: bool,
    /// Exit 6 when a client has new errors, or could not be read completely
    #[arg(long)]
    pub check: bool,
}

/// What `errors` does besides reading.
#[derive(Debug, Clone, clap::Subcommand)]
pub enum ErrorsAction {
    /// Mark the errors read as seen, every client's or -t's: they stop being new (it
    /// cannot be undone)
    #[command(after_long_help = DISMISS_HELP)]
    Dismiss(ErrorsDismissArgs),
}

/// `errors dismiss`' long help.
const DISMISS_HELP: &str = "\
Examples:
  omaddons errors dismiss -t retail
  omaddons errors dismiss -t retail --if-match pc1:…   only what that read showed

A dismissal only removes the \"new\" mark: the errors stay listed, and errors from
later sessions are new again. It cannot be undone. --if-match takes the token of an
errors read (targets[].dismiss.precondition) and acknowledges exactly what that read
showed, or refuses with plan_stale when a session ended since.";

/// `errors dismiss`' own flags (it takes `-t`, `--dry-run` and `--if-match`).
#[derive(Debug, Clone, Default, clap::Args)]
pub struct ErrorsDismissArgs {}

/// `errors dismiss`' data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct DismissData {
    pub dry_run: bool,
    pub targets: Vec<DismissTarget>,
    /// `--dry-run`: the token a run takes with `--if-match`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub precondition: Option<String>,
}

/// One client's dismissal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct DismissTarget {
    pub target: TargetRef,
    /// The databases acknowledged (or, with `--dry-run`, that would be), with the
    /// session acknowledged.
    pub acknowledged: Vec<AcknowledgedDb>,
    /// Databases read in part, over budget or unreadable: never acknowledged.
    pub skipped: Vec<SkippedDb>,
    /// New local errors that stop being new.
    pub errors: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct AcknowledgedDb {
    pub database: String,
    pub session: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct SkippedDb {
    pub database: String,
    pub reason: ReadState,
}

/// `errors dismiss`' outcome.
#[derive(Debug)]
pub struct DismissOutcome {
    data: DismissData,
    warnings: Vec<Warning>,
}

impl Outcome for DismissOutcome {
    type Data = DismissData;

    fn command(&self) -> &'static str {
        "errors dismiss"
    }

    fn data(&self) -> &DismissData {
        &self.data
    }

    fn warnings(&self) -> &[Warning] {
        &self.warnings
    }

    fn partial(&self) -> bool {
        self.data.targets.iter().any(|t| !t.skipped.is_empty())
    }

    fn render_human(&self, w: &mut dyn Write, _style: &Style) -> std::io::Result<()> {
        let verb = if self.data.dry_run {
            "would dismiss"
        } else {
            "dismissed"
        };
        for t in &self.data.targets {
            let n = t.acknowledged.len();
            if n == 0 && t.skipped.is_empty() {
                writeln!(w, "{}: no BugGrabber data to dismiss", t.target.id)?;
                continue;
            }
            if n > 0 {
                writeln!(
                    w,
                    "{}: {verb} {} new {} in {n} {}",
                    t.target.id,
                    t.errors,
                    plural(t.errors, "error", "errors"),
                    plural(n, "database", "databases")
                )?;
            }
            for s in &t.skipped {
                writeln!(
                    w,
                    "{}: {} was not read whole ({}); its errors stay as they are",
                    t.target.id,
                    s.database,
                    serde_json::to_value(s.reason)
                        .ok()
                        .and_then(|v| v.as_str().map(str::to_owned))
                        .unwrap_or_default()
                )?;
            }
        }
        if let Some(token) = &self.data.precondition {
            writeln!(w, "Run it bound to this preview: --if-match {token}")?;
        }
        Ok(())
    }
}

/// Runs `errors dismiss`: acknowledges each selected client's databases read whole at
/// their current session (M3-D19). With `--if-match`, only when that is exactly what the
/// token's read showed (M3-D46); checked before anything is written, and again under the
/// lock when a file moved.
pub fn dismiss(ctx: &Context) -> Result<DismissOutcome> {
    let now = crate::clock::now();
    let world = World::load(ctx)?;
    let mut warnings = world.warnings.clone();
    // A file that does not read is never overwritten: refused before anything else.
    let (acks, _) = ack::load_for_write(&world.paths)?;
    let (mut memo, writable) = memo::load(&world.paths);
    let preview = preview_argv(&ctx.argv);
    let mut seen = observe(ctx, &world, &mut memo, &acks, now, &mut warnings)?;
    let bound = |seen: &[Seen<'_>]| {
        let parts: Vec<DismissPart<'_>> = seen
            .iter()
            .map(|s| DismissPart {
                key: s.target.key,
                filed_as: &s.filed_as,
                observation: &s.observation,
            })
            .collect();
        of_dismiss(&parts)
    };
    // A dry run previews afresh, whatever token it is given: the current one is its
    // answer (README "Previews and --if-match").
    if ctx.dry_run {
        let token = bound(&seen).token();
        return Ok(DismissOutcome {
            data: DismissData {
                dry_run: true,
                targets: seen.iter().map(dismissal).collect(),
                precondition: Some(token),
            },
            warnings,
        });
    }
    check(ctx.if_match.as_deref(), &bound(&seen), &preview)?;
    let guard = GlobalLock::acquire(&world.paths, "errors dismiss")?;
    ctx.notes.warn_all(&recover_under(
        &guard,
        &world.paths,
        &no_saved_variables,
        ctx.command,
        ctx.actor,
    ));
    // A session that ended since the first look (the game wrote a file): look again,
    // under the lock, and bind to that.
    let moved = seen.iter().any(|s| {
        s.observation.databases.iter().any(|d| {
            FileStamp::read(&s.client.client_dir().join(&d.path))
                .ok()
                .flatten()
                != Some(d.stamp)
        })
    });
    if moved {
        seen = observe(ctx, &world, &mut memo, &acks, now, &mut Vec::new())?;
        check(ctx.if_match.as_deref(), &bound(&seen), &preview)?;
    }
    // Another dismissal may have written while this one waited for the lock: add to
    // what is on disk now, never to what was read before.
    let (mut acks, loaded_as) = ack::load_for_write(&world.paths)?;
    let targets: Vec<DismissTarget> = seen.iter().map(dismissal).collect();
    for s in &seen {
        for db in s
            .observation
            .databases
            .iter()
            .filter(|d| d.state.complete())
        {
            acks.targets
                .entry(s.filed_as.clone())
                .or_default()
                .databases
                .insert(
                    db_key(&db.path),
                    Ack {
                        acked_session: db.session.unwrap_or(0),
                        acked_at: now,
                    },
                );
        }
    }
    if targets.iter().any(|t| !t.acknowledged.is_empty()) {
        let scopes = ScopeTable::new(&world.paths, &[], None)?;
        let mut session = WriteSession::untracked(Writer::Mutating, &scopes, "errors-ack");
        ensure_dir_all(&mut session, world.paths.state_dir())?;
        ack::save(&mut session, &world.paths, &acks, loaded_as.as_ref())?;
        for (s, t) in seen.iter().zip(&targets) {
            if t.acknowledged.is_empty() {
                continue;
            }
            let entry = SettingEntry {
                dismissed: Some(Dismissed {
                    databases: t.acknowledged.len() as u64,
                    errors: t.errors as u64,
                }),
                ..SettingEntry::now(
                    TxKind::Dismiss,
                    s.target.key,
                    ctx.actor,
                    ctx.reason.clone(),
                    ctx.argv.clone(),
                )
            };
            if let Some(warning) = history::record_setting(&mut session, &world.paths, &entry) {
                ctx.notes.warn(warning.clone());
                warnings.push(warning);
            }
        }
        drop(guard);
        if writable {
            memo::store(&world.paths, &memo, ctx.actor);
        }
        crate::state::doorbell::ring(&world.paths, "errors dismiss", ctx.actor);
    }
    Ok(DismissOutcome {
        data: DismissData {
            dry_run: false,
            targets,
            precondition: None,
        },
        warnings,
    })
}

/// What a dismissal acknowledges on one client: its databases read whole, at their
/// current session, and the new errors that stop being new.
fn dismissal(s: &Seen<'_>) -> DismissTarget {
    let databases = &s.observation.databases;
    DismissTarget {
        target: TargetRef {
            id: s.target.id.clone(),
            key: s.target.key,
        },
        acknowledged: databases
            .iter()
            .filter(|d| d.state.complete())
            .map(|d| AcknowledgedDb {
                database: d.id.clone(),
                session: d.session,
            })
            .collect(),
        skipped: databases
            .iter()
            .filter(|d| !d.state.complete())
            .map(|d| SkippedDb {
                database: d.id.clone(),
                reason: d.state,
            })
            .collect(),
        errors: s
            .observation
            .local()
            .filter(|e| e.new && databases[e.database].state.complete())
            .count(),
    }
}

/// `errors`' data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct ErrorsData {
    /// `--raw-details`: each error carries its free text (`raw`).
    pub raw_details: bool,
    pub targets: Vec<ErrorsTarget>,
    pub summary: ErrorsSummary,
}

/// Every client together.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, JsonSchema)]
pub struct ErrorsSummary {
    pub targets: usize,
    /// New local errors.
    pub new: usize,
    /// Addons (and folders) with local errors.
    pub addons: usize,
    /// Errors `--since` could not place before or after its cutoff.
    pub uncertain: usize,
    pub received: usize,
}

/// One client's errors.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct ErrorsTarget {
    pub target: TargetRef,
    /// `buggrabber` when Lua errors are captured; `none` without BugGrabber.
    pub capture: CaptureState,
    /// Without BugGrabber, when the catalog knows no build of it for this client (so no
    /// `install_buggrabber` command is offered): why.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capture_note: Option<String>,
    /// `truncated` when a bound cut the walk for BugGrabber's files.
    pub coverage: CoverageState,
    pub files: Files,
    pub databases: Vec<DatabaseInfo>,
    pub summary: TargetSummary,
    /// By new errors, then occurrences, then name.
    pub addons: Vec<AddonInfo>,
    /// What `errors dismiss -t <id>` would acknowledge of what this read shows, and the
    /// token that binds it to this read (`--if-match`).
    pub dismiss: DismissInfo,
    /// When the newest of this client's valid dismissals was made; `null` when none.
    #[schemars(with = "Option<String>")]
    pub dismissed: Option<Timestamp>,
}

/// A dismissal's preview, from the read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct DismissInfo {
    /// `errors dismiss -t <id> --if-match <precondition>` acknowledges exactly what was
    /// read, or refuses with `plan_stale`.
    pub precondition: String,
    /// Databases read whole, which a dismissal would acknowledge.
    pub databases: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct TargetRef {
    pub id: String,
    pub key: Option<TargetKey>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CaptureState {
    Buggrabber,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CoverageState {
    Complete,
    Truncated,
}

/// The BugGrabber files found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Files {
    /// Read whole.
    pub read: usize,
    /// Account folders examined.
    pub accounts: usize,
    /// Read only in part, over budget, or unreadable.
    pub partial: usize,
    pub skipped: SkippedDirs,
}

/// `SavedVariables/` folders left alone, as undo leaves them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
pub struct SkippedDirs {
    pub symlinked: usize,
    pub two_spellings: usize,
}

/// One BugGrabber database (one file of one account, or of an embedding addon).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct DatabaseInfo {
    /// `db-<n>`: `errors[].database` names it.
    pub id: String,
    /// `account N`, never the folder name.
    pub account: String,
    /// `!BugGrabber`, or the addon that embeds the database.
    pub provider: String,
    pub shape: DbShape,
    pub session: Option<u32>,
    pub state: ReadState,
    /// Errors of later sessions are new.
    pub baseline: u32,
    /// The acknowledged session (`errors dismiss`); `null` when none.
    pub acknowledged: Option<u32>,
    /// `-v`: the file, relative to the client folder, its account as `account N`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

/// One client's counts. Totals come from every local error; `matched` and `shown` say
/// what the view's filters and `--limit` kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
pub struct TargetSummary {
    pub entries: usize,
    pub occurrences: u64,
    pub addons: usize,
    pub new: usize,
    pub matched: usize,
    pub shown: usize,
    pub uncertain: usize,
    pub no_time: usize,
    pub received: usize,
}

/// One addon's (or folder's) errors.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct AddonInfo {
    /// The managed addon, else the folder, else `Blizzard` or `unknown`.
    pub addon: String,
    pub managed: bool,
    pub folders: Vec<String>,
    pub kind: SourceKind,
    pub origin: Origin,
    /// A managed addon's version; `null` otherwise.
    pub version: Option<String>,
    /// What the release memo says is newest (no request); `null` when unmanaged.
    pub latest: Option<Latest>,
    pub count: u64,
    pub new: usize,
    pub entries: usize,
    pub last_seen: Option<TimeEvidence>,
    pub errors: Vec<ErrorInfo>,
}

/// One error (entries of one database with one fingerprint).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct ErrorInfo {
    pub fingerprint: String,
    pub database: String,
    pub origin: Origin,
    pub message_kind: MessageKind,
    /// A recognized template's text; `null` for a custom message.
    pub message: Option<String>,
    /// A custom message's length in characters.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message_chars: Option<usize>,
    pub location: Option<String>,
    pub attribution: AttributionInfo,
    pub stack: Vec<String>,
    pub count: u64,
    pub sessions: usize,
    pub new: bool,
    pub last_seen: Option<TimeEvidence>,
    #[serde(rename = "type")]
    pub entry_type: EntryType,
    /// `--raw-details`: the free text, made as safe as can be.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw: Option<RawInfo>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
pub struct AttributionInfo {
    pub method: Method,
    pub confidence: Confidence,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct RawInfo {
    pub message: String,
    pub stack: Vec<String>,
    pub locals: Option<String>,
    pub chunks: Vec<String>,
}

/// `errors`' outcome.
#[derive(Debug)]
pub struct ErrorsOutcome {
    data: ErrorsData,
    warnings: Vec<Warning>,
    commands: Vec<CommandHint>,
    failure: Option<Error>,
    /// Per target, for human output: `retail (12.1.5, mainline)`.
    labels: Vec<String>,
    running: Vec<String>,
    since: bool,
    /// For `-v`: why no dismissal applies, when the file does not read.
    pub notes: Vec<String>,
}

impl Outcome for ErrorsOutcome {
    type Data = ErrorsData;

    fn command(&self) -> &'static str {
        "errors"
    }

    fn data(&self) -> &ErrorsData {
        &self.data
    }

    fn warnings(&self) -> &[Warning] {
        &self.warnings
    }

    fn commands(&self) -> &[CommandHint] {
        &self.commands
    }

    fn assertion_failure(&self) -> Option<Error> {
        self.failure.clone()
    }

    fn render_human(&self, w: &mut dyn Write, _style: &Style) -> std::io::Result<()> {
        if self.data.targets.is_empty() {
            return writeln!(
                w,
                "No WoW client found. Add one with: omaddons targets add <path>"
            );
        }
        if self.data.raw_details {
            writeln!(
                w,
                "raw details may contain other players' names and your own code; share with care"
            )?;
        }
        for (t, label) in self.data.targets.iter().zip(&self.labels) {
            render_target(w, t, label)?;
        }
        if self.since {
            let (uncertain, no_time): (usize, usize) =
                self.data.targets.iter().fold((0, 0), |(u, n), t| {
                    (u + t.summary.uncertain, n + t.summary.no_time)
                });
            if uncertain + no_time > 0 {
                let mut parts = Vec::new();
                if uncertain > 0 {
                    parts.push(format!(
                        "{uncertain} older first-seen {} may have recurred since then (string times cannot tell)",
                        plural(uncertain, "error", "errors")
                    ));
                }
                if no_time > 0 {
                    parts.push(format!(
                        "{no_time} {} no time",
                        if no_time == 1 { "has" } else { "have" }
                    ));
                }
                writeln!(w, "{}", parts.join("; "))?;
            }
        }
        for id in &self.running {
            writeln!(
                w,
                "{id} is running: errors since the last /reload are not on disk yet"
            )?;
        }
        let updates: Vec<String> = self
            .commands
            .iter()
            .filter(|c| c.action() == "update")
            .map(|c| c.command().to_owned())
            .collect();
        if !updates.is_empty() {
            writeln!(w, "Update them: {}", updates.join(" · "))?;
        }
        for c in self
            .commands
            .iter()
            .filter(|c| c.action() == "install_buggrabber")
        {
            writeln!(w, "Capture Lua errors: {}", c.command())?;
        }
        Ok(())
    }
}

fn plural(n: usize, one: &str, many: &str) -> String {
    if n == 1 {
        one.to_owned()
    } else {
        many.to_owned()
    }
}

/// Why a client without BugGrabber is offered no install.
const NO_BUGGRABBER_BUILD: &str = "no BugGrabber build for this client is known";

fn render_target(w: &mut dyn Write, t: &ErrorsTarget, label: &str) -> std::io::Result<()> {
    if t.capture == CaptureState::None {
        let why = t
            .capture_note
            .as_deref()
            .unwrap_or("BugGrabber is not installed");
        return writeln!(w, "{label} · no Lua errors are captured: {why}");
    }
    let s = &t.summary;
    let mut head = vec![label.to_owned(), "BugGrabber".to_owned()];
    if s.entries == 0 {
        head.push("no errors".to_owned());
    } else {
        head.push(format!(
            "{} {} from {} {}",
            s.entries,
            plural(s.entries, "error", "errors"),
            s.addons,
            plural(s.addons, "addon", "addons")
        ));
        if s.new > 0 {
            head.push(format!("{} new", s.new));
        }
    }
    if s.received > 0 {
        head.push(format!("{} received (not counted)", s.received));
    }
    if t.coverage == CoverageState::Truncated || t.files.partial > 0 {
        head.push("read in part".to_owned());
    }
    writeln!(w, "{}", head.join(" · "))?;
    let width = t
        .addons
        .iter()
        .map(|a| a.addon.chars().count())
        .max()
        .unwrap_or(0);
    for a in &t.addons {
        let mut line = format!("  {:<width$}  {}×", a.addon, a.count);
        if a.origin == Origin::Received {
            line.push_str("  received");
        } else if a.new > 0 {
            line.push_str(&format!("  {} new", a.new));
        }
        if let Some(seen) = &a.last_seen {
            line.push_str(&format!("  {}", seen_text(seen)));
        }
        line.push_str(&format!("  {}", source_text(a)));
        if let Some(latest) = latest_text(a) {
            line.push_str(&format!("  {latest}"));
        }
        writeln!(w, "{line}")?;
        for e in &a.errors {
            let message = match &e.message {
                Some(text) => text.clone(),
                None => format!(
                    "custom error message ({} characters; --raw-details shows it)",
                    e.message_chars.unwrap_or(0)
                ),
            };
            match &e.location {
                Some(at) => writeln!(w, "    {at}: {message}")?,
                None => writeln!(w, "    {message}")?,
            }
            for frame in &e.stack {
                writeln!(w, "      {frame}")?;
            }
            if let Some(raw) = &e.raw {
                writeln!(w, "      raw: {}", raw.message)?;
                for line in &raw.stack {
                    writeln!(w, "        {line}")?;
                }
                if let Some(locals) = &raw.locals {
                    for line in locals.lines() {
                        writeln!(w, "        {line}")?;
                    }
                }
            }
        }
    }
    Ok(())
}

/// When an error was last seen, no more precise than its evidence: a string time is
/// only a lower bound, read in this machine's zone.
fn seen_text(seen: &TimeEvidence) -> String {
    let Some(at) = seen.at else {
        return String::new();
    };
    match seen.meaning {
        TimeMeaning::Last => format!("last {}", local_time(at)),
        TimeMeaning::FirstOrLast | TimeMeaning::Unknown => format!("last ≥ {}", local_time(at)),
    }
}

/// The attribution, worded as a likelihood (M3-D18).
fn source_text(a: &AddonInfo) -> &'static str {
    let top = a.errors.first();
    match top.map(|e| e.attribution.method) {
        Some(Method::ProtectedCall) => "blocked call from",
        _ if top.is_some_and(|e| {
            e.location
                .as_deref()
                .is_some_and(|l| l.starts_with("[custom code]"))
                || e.stack.iter().any(|f| f == "[custom code]")
        }) =>
        {
            "likely source (custom code)"
        }
        _ if a.kind == SourceKind::Library => "likely source (a library it embeds)",
        _ => "likely source",
    }
}

/// An update offered as something that may fix it, never a promise.
fn latest_text(a: &AddonInfo) -> Option<String> {
    let latest = a.latest.as_ref()?;
    match latest.status {
        StatusKind::UpdateAvailable => Some(format!(
            "{} → {} available (may fix it)",
            a.version.as_deref().unwrap_or("?"),
            latest.version.as_deref().unwrap_or("?")
        )),
        StatusKind::Current => Some("current".to_owned()),
        _ => None,
    }
}

/// Runs `errors`.
pub fn run(ctx: &Context, args: &ErrorsArgs) -> Result<ErrorsOutcome> {
    let now = crate::clock::now();
    let since = args
        .since
        .as_deref()
        .map(|s| {
            parse_since(s, now).ok_or_else(|| {
                Error::new(
                    ErrorCode::Usage,
                    format!(
                        "--since takes a duration (30m, 12h, 1d, 2w) or an RFC 3339 time, not {s:?}"
                    ),
                    Suggestion::command(["omaddons", "errors", "--since", "1d"]),
                )
            })
        })
        .transpose()?;
    let limit = args.limit.unwrap_or(DEFAULT_LIMIT);
    if limit == 0 {
        return Err(Error::new(
            ErrorCode::Usage,
            "--limit must be at least 1",
            Suggestion::command(["omaddons", "errors", "--limit", "10"]),
        ));
    }
    let world = World::load(ctx)?;
    let verbose = ctx.output.style.verbose > 0;
    let mut warnings = world.warnings.clone();
    let remembered = Remembered::load(&world);
    let (mut memo, writable) = memo::load(&world.paths);
    let (acks, ack_note) = ack::read(&world.paths);
    let seen = observe(ctx, &world, &mut memo, &acks, now, &mut warnings)?;
    let found: Vec<Target> = seen.iter().map(|s| s.target.clone()).collect();
    let running_scan = detect_running(&found);
    let (catalog, catalog_warnings) = world.catalog(ctx);
    warnings.extend(catalog_warnings);
    let mut view = View {
        since,
        addon: args.addon.as_deref(),
        limit,
        received: args.received,
        raw_details: args.raw_details,
        check: args.check,
        verbose,
        catalog: &catalog,
        commands: Vec::new(),
        failure: None,
    };
    let mut targets = Vec::new();
    let mut labels = Vec::new();
    let mut running = Vec::new();
    for s in &seen {
        let t = s.target;
        let latest =
            |addon: &str| latest_of(&remembered, t, s.record.as_ref(), &world.table, addon);
        let names = WtfPathRedactor::for_client(&s.client);
        targets.push(view.target(t, &s.filed_as, &s.observation, &latest, &names));
        labels.push(format!("{} ({}, {})", t.id, t.version, t.family));
        if running_scan
            .clients
            .get(&t.client_dir)
            .is_some_and(|r| !r.is_empty())
        {
            running.push(t.id.clone());
        }
    }
    if writable {
        memo::store(&world.paths, &memo, ctx.actor);
    }
    if let Some(addon) = &args.addon
        && !targets
            .iter()
            .any(|t| t.capture == CaptureState::Buggrabber)
    {
        return Err(Error::new(
            ErrorCode::NotFound,
            format!("no selected client captures Lua errors, so {addon:?} has none to show"),
            Suggestion::command(["omaddons", "search", "buggrabber"]),
        ));
    }
    let summary = ErrorsSummary {
        targets: targets.len(),
        new: targets.iter().map(|t| t.summary.new).sum(),
        addons: targets.iter().map(|t| t.summary.addons).sum(),
        uncertain: targets.iter().map(|t| t.summary.uncertain).sum(),
        received: targets.iter().map(|t| t.summary.received).sum(),
    };
    Ok(ErrorsOutcome {
        data: ErrorsData {
            raw_details: args.raw_details,
            targets,
            summary,
        },
        warnings,
        commands: view.commands,
        failure: view.failure,
        labels,
        running,
        since: args.since.is_some(),
        notes: ack_note.into_iter().collect(),
    })
}

/// One client as `errors` saw it.
struct Seen<'w> {
    target: &'w Target,
    /// Where its memo and dismissals are filed: its key, or its id until registered.
    filed_as: String,
    client: ClientPaths,
    record: Option<TargetRecord>,
    observation: Observation,
}

/// Every selected client's errors, observed whole (before any view filter), each one's
/// memo entry replaced by what was seen.
fn observe<'w>(
    ctx: &Context,
    world: &'w World,
    memo: &mut ErrorsMemo,
    acks: &ErrorsAck,
    now: Timestamp,
    warnings: &mut Vec<Warning>,
) -> Result<Vec<Seen<'w>>> {
    let tz = TimeZone::system();
    let mut budget = ERRORS_MAX_TOTAL_BYTES;
    let mut seen = Vec::new();
    for d in world.select_many(ctx)? {
        let t = &d.target;
        let client = ClientPaths::new(&t.client_dir);
        let record = match t.key {
            Some(key) => {
                let loaded = load_record(&world.paths, &key)?;
                let warning = loaded.warning(&world.paths.target_record(&key));
                ctx.notes.warn_all(warning.as_slice());
                warnings.extend(warning);
                Some(loaded.value)
            }
            None => None,
        };
        let shared = world.sibling_records(t)?;
        let inventory = inventory_shared(t, record.as_ref(), &shared, &world.table, Depth::Full)?;
        let owner = |folder: &str| owner_of(record.as_ref(), folder);
        let filed_as = t.key.map_or_else(|| t.id.clone(), |k| k.to_string());
        let observation = read_target(
            &Observe {
                target: &t.id,
                client: &client,
                inventory: &inventory,
                owner: &owner,
                memo: memo.targets.get(&filed_as),
                ack: acks.targets.get(&filed_as),
                now,
                tz: &tz,
            },
            &mut budget,
        );
        let fresh = target_memo(&observation, memo.targets.get(&filed_as));
        memo.targets.insert(filed_as.clone(), fresh);
        ctx.notes.warn_all(&observation.warnings);
        warnings.extend(observation.warnings.iter().cloned());
        seen.push(Seen {
            target: t,
            filed_as,
            client,
            record,
            observation,
        });
    }
    Ok(seen)
}

/// The managed addon whose record lists `folder`.
pub(crate) fn owner_of(record: Option<&TargetRecord>, folder: &str) -> Option<String> {
    let key = folder.to_ascii_lowercase();
    record?
        .addons
        .values()
        .find(|a| a.folders.iter().any(|f| f.key() == key))
        .map(|a| a.name.clone())
}

/// A managed addon's version and what the release memo says is newest.
fn latest_of(
    remembered: &Remembered,
    target: &Target,
    record: Option<&TargetRecord>,
    table: &crate::rules::RulesTable,
    addon: &str,
) -> Option<(String, Latest)> {
    let record = record?;
    let a = record.addons.get(&TargetRecord::addon_key(addon))?;
    let rows = remembered.statuses(target, record, table);
    let status = rows
        .iter()
        .find(|r| r.addon.eq_ignore_ascii_case(addon))
        .map(|r| &r.status);
    Some((
        a.version.clone(),
        Latest {
            version: status
                .and_then(UpdateStatus::latest)
                .map(|l| l.version.clone()),
            status: status.map_or(StatusKind::Unchecked, UpdateStatus::kind),
        },
    ))
}

/// The view's filters, and what it accumulates across targets.
struct View<'a> {
    since: Option<jiff::Timestamp>,
    addon: Option<&'a str>,
    limit: usize,
    received: bool,
    raw_details: bool,
    check: bool,
    verbose: bool,
    /// Where BugGrabber comes from on each client (M3-D16).
    catalog: &'a Catalog,
    commands: Vec<CommandHint>,
    failure: Option<Error>,
}

/// Where an entry falls against `--since`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Since {
    Matched,
    Uncertain,
    NoTime,
    Before,
}

impl View<'_> {
    fn chosen(&self, e: &Observed) -> bool {
        self.addon.is_none_or(|addon| {
            e.name.eq_ignore_ascii_case(addon)
                || e.folder
                    .as_deref()
                    .is_some_and(|f| f.eq_ignore_ascii_case(addon))
        })
    }

    fn since(&self, e: &Observed) -> Since {
        let Some(cutoff) = self.since else {
            return Since::Matched;
        };
        match e.evidence.at_or_after(cutoff) {
            Tri::Yes => Since::Matched,
            Tri::No => Since::Before,
            Tri::Uncertain if e.evidence.at.is_some() => Since::Uncertain,
            Tri::Uncertain => Since::NoTime,
        }
    }

    fn target(
        &mut self,
        t: &Target,
        filed_as: &str,
        observation: &Observation,
        latest: &dyn Fn(&str) -> Option<(String, Latest)>,
        names: &WtfPathRedactor,
    ) -> ErrorsTarget {
        let local: Vec<&Observed> = observation.local().collect();
        let chosen: Vec<&Observed> = local.iter().copied().filter(|e| self.chosen(e)).collect();
        let placed: Vec<(&Observed, Since)> = chosen.iter().map(|e| (*e, self.since(e))).collect();
        let matched: Vec<&Observed> = placed
            .iter()
            .filter(|(_, s)| *s == Since::Matched)
            .map(|(e, _)| *e)
            .collect();
        let count = |which: Since| placed.iter().filter(|(_, s)| *s == which).count();
        let mut groups = group(&matched);
        groups.truncate(self.limit);
        if self.received {
            let received: Vec<&Observed> = observation
                .received()
                .filter(|e| self.chosen(e))
                .filter(|e| self.since(e) != Since::Before)
                .collect();
            let mut received = group(&received);
            received.truncate(self.limit);
            groups.extend(received);
        }
        let shown = groups
            .iter()
            .filter(|g| g.origin == Origin::Local)
            .map(|g| g.entries)
            .sum();
        let new_local = local.iter().filter(|e| e.new).count();
        // The check: new errors among those the selection keeps (an uncertain time
        // counts, M3-D22), or a read it cannot certify; never `--limit`.
        if self.check && self.failure.is_none() {
            let new_selected = placed
                .iter()
                .filter(|(e, s)| {
                    e.new && matches!(s, Since::Matched | Since::Uncertain | Since::NoTime)
                })
                .count();
            let fix = Suggestion::command(["omaddons", "errors", "-t", t.id.as_str()]);
            if !observation.complete() {
                self.failure = Some(
                    Error::new(
                        ErrorCode::CheckFailed,
                        format!("{}: BugGrabber's data could not be read completely", t.id),
                        fix,
                    )
                    .details(serde_json::json!({"reason": "incomplete", "target": t.id})),
                );
            } else if new_selected > 0 {
                self.failure = Some(
                    Error::new(
                        ErrorCode::CheckFailed,
                        format!(
                            "{} has {new_selected} new Lua {}",
                            t.id,
                            plural(new_selected, "error", "errors")
                        ),
                        fix,
                    )
                    .details(serde_json::json!({"reason": "new_errors", "target": t.id})),
                );
            }
        }
        // Updates that may fix new errors: managed addons with new local errors and a
        // newer release known (received errors never count).
        let mut to_update: Vec<String> = Vec::new();
        for g in group(&chosen) {
            if g.managed
                && g.new > 0
                && let Some((_, l)) = latest(&g.name)
                && l.status == StatusKind::UpdateAvailable
                && !to_update.iter().any(|u| u.eq_ignore_ascii_case(&g.name))
            {
                to_update.push(g.name.clone());
            }
        }
        if !to_update.is_empty() {
            let mut argv = vec!["omaddons".to_owned(), "update".to_owned()];
            argv.extend(to_update);
            argv.extend(["-t".to_owned(), t.id.clone()]);
            self.commands.push(CommandHint::new("update", argv, false));
        }
        // A dismissal acknowledges only databases read whole: offered when it would clear
        // something.
        let dismissable = local
            .iter()
            .any(|e| e.new && observation.databases[e.database].state.complete());
        if dismissable {
            self.commands.push(CommandHint::new(
                "dismiss_errors",
                ["omaddons", "errors", "dismiss", "-t", t.id.as_str()],
                false,
            ));
        }
        // Without BugGrabber: the catalog's build for this client, or none known (never
        // another family's).
        let mut capture_note = None;
        if observation.capture == Capture::None {
            if self.catalog.resolve("buggrabber", Some(t.family)).is_some() {
                self.commands.push(CommandHint::new(
                    "install_buggrabber",
                    ["omaddons", "install", "buggrabber", "-t", t.id.as_str()],
                    false,
                ));
            } else {
                capture_note = Some(NO_BUGGRABBER_BUILD.to_owned());
            }
        }
        let dismiss = DismissInfo {
            precondition: of_dismiss(&[DismissPart {
                key: t.key,
                filed_as,
                observation,
            }])
            .token(),
            databases: observation
                .databases
                .iter()
                .filter(|d| d.state.complete())
                .count(),
        };
        let dismissed = observation
            .databases
            .iter()
            .filter_map(|d| d.acknowledged.map(|a| a.acked_at))
            .max();
        let databases = observation
            .databases
            .iter()
            .map(|db| DatabaseInfo {
                id: db.id.clone(),
                account: format!("account {}", db.account),
                provider: db.provider.clone(),
                shape: db.shape,
                session: db.session,
                state: db.state,
                baseline: db.baseline,
                acknowledged: db.acknowledged.map(|a| a.acked_session),
                path: self.verbose.then(|| names.rel(&db.path)),
            })
            .collect();
        let addons = groups
            .iter()
            .map(|g| self.addon_info(g, observation, latest, names))
            .collect();
        ErrorsTarget {
            target: TargetRef {
                id: t.id.clone(),
                key: t.key,
            },
            capture: match observation.capture {
                Capture::BugGrabber => CaptureState::Buggrabber,
                Capture::None => CaptureState::None,
            },
            capture_note,
            coverage: match observation.coverage {
                Coverage::Complete => CoverageState::Complete,
                Coverage::Truncated { .. } => CoverageState::Truncated,
            },
            files: Files {
                read: observation
                    .databases
                    .iter()
                    .filter(|d| d.state == ReadState::Complete)
                    .count(),
                accounts: match observation.coverage {
                    Coverage::Truncated { accounts, .. } => accounts,
                    Coverage::Complete => observation
                        .databases
                        .iter()
                        .map(|d| d.account)
                        .max()
                        .unwrap_or(0)
                        .max(
                            observation
                                .listings
                                .iter()
                                .map(|l| l.account)
                                .max()
                                .unwrap_or(0),
                        ),
                },
                partial: observation
                    .databases
                    .iter()
                    .filter(|d| d.state != ReadState::Complete)
                    .count(),
                skipped: SkippedDirs {
                    symlinked: observation.skipped.symlinked,
                    two_spellings: observation.skipped.two_spellings,
                },
            },
            databases,
            summary: TargetSummary {
                entries: local.len(),
                occurrences: local.iter().map(|e| u64::from(e.entry.counter)).sum(),
                addons: group(&local).len(),
                new: new_local,
                matched: matched.len(),
                shown,
                uncertain: count(Since::Uncertain),
                no_time: count(Since::NoTime),
                received: observation.received().count(),
            },
            addons,
            dismiss,
            dismissed,
        }
    }

    fn addon_info(
        &self,
        g: &AddonErrors<'_>,
        observation: &Observation,
        latest: &dyn Fn(&str) -> Option<(String, Latest)>,
        names: &WtfPathRedactor,
    ) -> AddonInfo {
        let (version, latest) = match g.managed.then(|| latest(&g.name)).flatten() {
            Some((version, latest)) => (Some(version), Some(latest)),
            None => (None, None),
        };
        AddonInfo {
            addon: g.name.clone(),
            managed: g.managed,
            folders: g.folders.clone(),
            kind: g.kind,
            origin: g.origin,
            version,
            latest,
            count: g.count,
            new: g.new,
            entries: g.entries,
            last_seen: shown_time(g.last_seen),
            // New first, then the most frequent (`group`'s order); `entries` counts all.
            errors: g
                .errors
                .iter()
                .take(ERRORS_SHOWN_PER_ADDON)
                .map(|e| {
                    let entry = &e.shown.entry;
                    let strict = strict_message(&entry.message);
                    let db = &observation.databases[e.shown.database];
                    // r152 keeps its frames in the message, after the first line.
                    let stack = match (&entry.stack, db.shape) {
                        (Some(stack), _) => stack.as_str(),
                        (None, DbShape::Legacy) => {
                            entry.message.split_once('\n').map_or("", |(_, rest)| rest)
                        }
                        (None, DbShape::Modern) => "",
                    };
                    ErrorInfo {
                        fingerprint: e.shown.fingerprint.clone(),
                        database: db.id.clone(),
                        origin: entry.origin,
                        message_kind: strict.kind,
                        message_chars: (strict.kind == MessageKind::Custom).then_some(strict.chars),
                        message: strict.text,
                        location: strict.location,
                        attribution: AttributionInfo {
                            method: e.shown.attribution.method,
                            confidence: e.shown.attribution.confidence,
                        },
                        stack: strict_frames(stack, self.verbose),
                        count: e.count,
                        sessions: e.sessions.len(),
                        new: e.new,
                        last_seen: shown_time(e.last_seen),
                        entry_type: entry.entry_type,
                        raw: self.raw_details.then(|| {
                            let raw = raw_details(entry, names);
                            RawInfo {
                                message: raw.message,
                                stack: raw.stack,
                                locals: raw.locals,
                                chunks: raw.chunks,
                            }
                        }),
                    }
                })
                .collect(),
        }
    }
}
