//! The typed intent of a stored plan (M4-D23): what `omaddons apply <plan_id>` executes,
//! one variant per storable command, each holding that command's arguments as the
//! preview normalized them. Every path is absolute, every target the preview resolved is
//! named by its key (or its provisional id while unregistered), and every relative input
//! (a transaction prefix, "the most recent run", an age) is the absolute value the
//! preview used, so a plan means the same wherever and whenever it is applied.
//!
//! Decoding admits only what agrees with itself: an unknown command, an unknown field, a
//! relative or non-normalized path, or a ref no person could have typed is no intent, and
//! [`StoredIntent::validate`] checks the rules decoding cannot express. `display_argv`
//! is derived from the intent, for people and history; nothing parses or dispatches it.

use std::borrow::Cow;
use std::fmt;
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};

use jiff::Timestamp;
use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Serialize};

use crate::app::adopt::AdoptArgs;
use crate::app::approval::{Approval, ApprovalFlags};
use crate::app::backup::{BackupPruneArgs, BackupRestoreArgs};
use crate::app::copy::CopyArgs;
use crate::app::install::InstallArgs;
use crate::app::pin::{PinArgs, UnpinArgs};
use crate::app::remove::RemoveArgs;
use crate::app::sync::SyncArgs;
use crate::app::undo::UndoArgs;
use crate::app::update::UpdateArgs;
use crate::cli::Context;
use crate::engine::SvPolicy;
use crate::error::{Error, Result, Suggestion, Warning, WarningCode};
use crate::model::{BackupId, Channel, PlanId, Ref, RefKind, RunId, Target, TargetRef, TxId};
use crate::output::{Outcome, Style};
use crate::sources::refs::{MatchKind, ParsedRef};
use crate::state::paths::Paths;
use crate::state::plans::{
    self, Admission, NotStored, StoredParts, StoredPlan, StoredPrecondition, StoredRelease,
};

/// What a storable dry run hands the plan store; never serialized.
#[derive(Debug)]
pub struct Preview {
    pub intent: StoredIntent,
    pub precondition: super::precondition::Precondition,
    pub releases: Vec<StoredRelease>,
    pub approval: Approval,
    pub changes: bool,
}

/// A command outcome whose direct invocation may store its preview. `apply` calls
/// command functions directly and never passes through this dispatch-only offer.
pub trait Storable: Outcome {
    fn take_preview(&mut self) -> Option<Preview>;
    fn record_plan(&mut self, plan_id: PlanId, expires_at: Timestamp);
}

/// A command's final plan and the exact precondition it returned, before the plan
/// goes out of scope. Intent normalization may decline storage; a broken approval
/// invariant remains an error in the command's plan.
pub fn preview_plan(
    ctx: &Context,
    args: &StorableArgs<'_>,
    precondition: super::precondition::Precondition,
    plan: &crate::engine::plan::Plan,
    before: &std::collections::BTreeMap<String, crate::state::records::TargetRecord>,
) -> Result<Option<Box<Preview>>> {
    let intent = match StoredIntent::from_args(ctx, args) {
        Ok(intent) => intent,
        Err(err) => {
            ctx.notes.warn(plan_not_stored(&NotStored::Io(format!(
                "this preview cannot be stored: {}",
                err.message,
            ))));
            return Ok(None);
        }
    };
    let approval = Approval::of_plan(plan, before, &intent.approval_flags())?;
    Ok(Some(Box::new(Preview {
        intent,
        precondition,
        releases: StoredRelease::of_plan(plan),
        approval,
        changes: plan.has_ops(),
    })))
}

/// Stores a preview that would change something; returns its `-v` note. Failure to
/// keep a plan never fails the preview or takes away its binding token.
pub fn offer<O: Storable>(ctx: &Context, outcome: &mut O) -> Option<String> {
    offer_with_paths(ctx, outcome, Paths::from_env)
}

fn offer_with_paths<O: Storable>(
    ctx: &Context,
    outcome: &mut O,
    paths: impl FnOnce() -> Result<Paths>,
) -> Option<String> {
    let preview = outcome.take_preview()?;
    // `apply --dry-run` writes nothing, not even a plan of its own (M4-D23): the plan it
    // previews is the stored one. The outcome keeps its approval.
    if ctx.applying.is_some() {
        return Some("plan store: nothing stored while applying a stored plan".into());
    }
    if !preview.changes {
        return Some("plan store: nothing to store".into());
    }
    let mut report = plans::AdmissionReport::default();
    let admission = (|| {
        let mut human = Vec::new();
        outcome
            .render_human(&mut human, &Style::default())
            .map_err(|err| NotStored::Io(err.to_string()))?;
        let summary = String::from_utf8(human)
            .map_err(|err| NotStored::Io(err.to_string()))?
            .lines()
            .map(str::to_owned)
            .collect();
        let created_at = crate::clock::now();
        let ttl = jiff::SignedDuration::try_from(crate::limits::PLAN_TTL)
            .map_err(|err| NotStored::Io(err.to_string()))?;
        let expires_at = created_at
            .checked_add(ttl)
            .map_err(|err| NotStored::Io(err.to_string()))?;
        let plan = StoredPlan {
            plan_id: PlanId::new(),
            created_at,
            expires_at,
            tool_version: env!("CARGO_PKG_VERSION").into(),
            build_id: crate::build_info::BUILD_ID.into(),
            display_argv: preview.intent.display_argv(),
            intent: preview.intent,
            actor: ctx.actor,
            reason: ctx.reason.clone(),
            precondition: StoredPrecondition {
                token: preview.precondition.token(),
                parts: StoredParts::of(&preview.precondition),
            },
            releases: preview.releases,
            approval: preview.approval,
            summary,
        };
        let paths = paths().map_err(|err| NotStored::Io(err.message.clone()))?;
        let (admission, read) = plans::store(&paths, &plan);
        report = read;
        Ok(admission)
    })()
    .unwrap_or_else(Admission::NotStored);
    let result = match admission {
        Admission::Stored {
            plan_id,
            expires_at,
            reused,
        } => {
            outcome.record_plan(plan_id, expires_at);
            format!("{} {plan_id}", if reused { "reused" } else { "stored" })
        }
        Admission::NotStored(why) => {
            ctx.notes.warn(plan_not_stored(&why));
            format!("not stored ({why:?})")
        }
    };
    Some(format!(
        "plan store: listed {} plans ({}), swept {}, skipped {}, read {}; {result}",
        report.listed,
        if report.complete { "complete" } else { "cut" },
        report.sweep.removed,
        report.sweep.skipped_unreadable,
        report.read,
    ))
}

/// One cause-specific warning, shared by intent normalization and store admission.
pub fn plan_not_stored(why: &NotStored) -> Warning {
    let message = match why {
        NotStored::Full { plans, bytes } => format!(
            "the plan store is full ({plans} plans, {bytes} bytes): this preview was not stored; expired plans are cleared as they age, within 24 hours; the preview and its token stand"
        ),
        NotStored::TooLarge { .. } =>
            "this preview's plan is larger than 256 KiB and was not stored; the preview and its token stand".into(),
        NotStored::Busy =>
            "the plan store stayed locked by another preview for 5 s; this preview was not stored; the preview and its token stand".into(),
        NotStored::Io(err) => format!(
            "the plan could not be stored ({err}); the preview and its token stand"
        ),
    };
    Warning::new(WarningCode::PlanNotStored, message).with_fix(Suggestion::text(
        "stored plans are kept in omaddons' cache directory, which is safe to empty",
    ))
}

/// Ends human output only when a plan was kept, with its original expiry on reuse.
pub fn render_plan_line(
    w: &mut dyn Write,
    plan_id: Option<&PlanId>,
    expires_at: Option<&Timestamp>,
) -> io::Result<()> {
    render_plan_line_at(w, plan_id, expires_at, crate::clock::now())
}

fn render_plan_line_at(
    w: &mut dyn Write,
    plan_id: Option<&PlanId>,
    expires_at: Option<&Timestamp>,
    now: Timestamp,
) -> io::Result<()> {
    if let (Some(id), Some(expiry)) = (plan_id, expires_at) {
        let seconds = expiry
            .since(now)
            .map_err(io::Error::other)?
            .get_seconds()
            .max(0);
        let (n, unit) = if seconds >= 3_600 {
            ((seconds + 1_800) / 3_600, "hour")
        } else {
            (((seconds + 30) / 60).max(1), "minute")
        };
        writeln!(
            w,
            "Plan {id} stored for {n} {unit}{}.",
            if n == 1 { "" } else { "s" }
        )?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------
// Building blocks.

/// An absolute, normalized UTF-8 path: no `..`, no `.`, no trailing or doubled slash. It
/// decodes only in that form, so a stored plan names the file it names wherever it is
/// read.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct AbsPath(String);

impl AbsPath {
    /// `input` as an absolute path against `cwd`, exactly as the command planned it
    /// (`sources::refs::absolute`): a `..` left by the join resolves through the
    /// filesystem (the input exists at preview; a missing one already failed it, and one
    /// whose `..` does not resolve is refused here).
    pub fn resolve(cwd: &Path, input: &Path) -> std::result::Result<Self, String> {
        let text = crate::sources::refs::absolute(cwd, input)
            .into_os_string()
            .into_string()
            .map_err(|_| "this path cannot be stored in a plan: it is not UTF-8".to_owned())?;
        Self::new(text)
    }

    /// The path, checked.
    pub fn new(text: String) -> std::result::Result<Self, String> {
        let path = Path::new(&text);
        let normal = path.components().collect::<PathBuf>();
        if !path.is_absolute()
            || text.contains('\0')
            || path
                .components()
                .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
            || normal.as_os_str() != text.as_str()
        {
            return Err(format!("{text:?} is not an absolute, normalized path"));
        }
        Ok(Self(text))
    }

    pub fn as_path(&self) -> &Path {
        Path::new(&self.0)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for AbsPath {
    type Error = String;

    fn try_from(text: String) -> std::result::Result<Self, String> {
        Self::new(text)
    }
}

impl From<AbsPath> for String {
    fn from(path: AbsPath) -> Self {
        path.0
    }
}

impl fmt::Display for AbsPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl JsonSchema for AbsPath {
    fn schema_name() -> Cow<'static, str> {
        "AbsPath".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({ "type": "string" })
    }
}

/// One ref-like argument, stored as the plain string a person would type: a canonical
/// ref, `file:` and an absolute path, an `https://` link (keeping a pasted release link's
/// version and asset), or a bare name (an installed addon, or a catalog name or alias,
/// which each client resolves to its own variant at plan time, M3-D16).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub enum NormalizedRef {
    /// `file:<absolute path>`
    File(AbsPath),
    /// A canonical ref of another source (`gh:`, `git:`, `url:`, `wowi:`, …).
    Canonical(String),
    /// A pasted `https://` link.
    Link(String),
    /// A bare name.
    Name(String),
}

impl NormalizedRef {
    /// The ref a run parsed from `request`, as a plan stores it.
    pub fn from_parsed(request: &str, parsed: &ParsedRef) -> std::result::Result<Self, String> {
        let file = |r: &Ref| match r {
            Ref::File { path } => AbsPath::resolve(Path::new("/"), path).map(Self::File),
            other => Err(format!("{other} is not a local path")),
        };
        match parsed.matched_by {
            MatchKind::Path => file(&parsed.r#ref),
            MatchKind::Canonical if matches!(parsed.r#ref, Ref::File { .. }) => file(&parsed.r#ref),
            MatchKind::Shorthand => Self::parse(&parsed.r#ref.to_string()),
            MatchKind::Canonical | MatchKind::Url | MatchKind::Catalog | MatchKind::Installed => {
                Self::parse(request.trim())
            }
        }
    }

    /// The stored string read back: only the four forms, each checked.
    fn parse(text: &str) -> std::result::Result<Self, String> {
        let refuse = |why: &str| Err(format!("{text:?} cannot be a stored ref: {why}"));
        if text.trim() != text || text.is_empty() || text.chars().any(|c| c.is_ascii_control()) {
            return refuse("it is empty, padded or holds a control character");
        }
        if let Some(path) = text.strip_prefix("file:") {
            return AbsPath::new(path.to_owned()).map(Self::File);
        }
        if let Some((prefix, _)) = text.split_once(':')
            && RefKind::ALL.iter().any(|k| k.prefix() == prefix)
        {
            return match text.parse::<Ref>() {
                Ok(_) => Ok(Self::Canonical(text.to_owned())),
                Err(err) => refuse(&err.to_string()),
            };
        }
        if let Some(rest) = text.strip_prefix("https://") {
            return if rest.is_empty() || text.contains(char::is_whitespace) {
                refuse("it is not a link")
            } else {
                Ok(Self::Link(text.to_owned()))
            };
        }
        if text.contains('/') || text.starts_with(['.', '~', '-']) {
            return refuse("a name holds no '/' and does not start with '.', '~' or '-'");
        }
        Ok(Self::Name(text.to_owned()))
    }

    /// The local archive or folder it names, for `apply`'s `input_missing` check.
    pub fn file(&self) -> Option<&AbsPath> {
        match self {
            Self::File(path) => Some(path),
            Self::Canonical(_) | Self::Link(_) | Self::Name(_) => None,
        }
    }
}

impl TryFrom<String> for NormalizedRef {
    type Error = String;

    fn try_from(text: String) -> std::result::Result<Self, String> {
        Self::parse(&text)
    }
}

impl From<NormalizedRef> for String {
    fn from(r: NormalizedRef) -> Self {
        r.to_string()
    }
}

impl fmt::Display for NormalizedRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::File(path) => write!(f, "file:{path}"),
            Self::Canonical(text) | Self::Link(text) | Self::Name(text) => f.write_str(text),
        }
    }
}

impl JsonSchema for NormalizedRef {
    fn schema_name() -> Cow<'static, str> {
        "NormalizedRef".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({ "type": "string" })
    }
}

/// A client the preview resolved: its key (or provisional id), and the id it had then,
/// which only `display_argv` uses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct IntentTarget {
    pub target: TargetRef,
    pub target_id: String,
}

impl IntentTarget {
    pub fn of(target: &Target) -> Self {
        Self {
            target: TargetRef::of(target.key, &target.id),
            target_id: target.id.clone(),
        }
    }
}

/// The clients a command over several selected: `apply` acts on exactly `targets`,
/// never resolving `selector` again, so a client found after the preview is never one a
/// plan touches. `all` (no `-t`, or `-t all`) and `selector` (`-t` as typed, a path
/// canonicalized) shape only `display_argv`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TargetScope {
    pub all: bool,
    pub selector: Option<String>,
    /// Sorted by `target_id`, one entry per target.
    pub targets: Vec<IntentTarget>,
}

impl TargetScope {
    /// The scope of a run given `-t selector` that selected `selected`.
    pub fn of(selector: Option<&str>, selected: &[&Target]) -> Self {
        let mut targets: Vec<IntentTarget> = Vec::new();
        for target in selected {
            let target = IntentTarget::of(target);
            if !targets.iter().any(|t| t.target == target.target) {
                targets.push(target);
            }
        }
        targets.sort_by(|a, b| a.target_id.cmp(&b.target_id));
        // A target named `all` wins over the keyword (`target_id_reserved`).
        let keyword = selector.is_some_and(|s| {
            s.eq_ignore_ascii_case("all") && !targets.iter().any(|t| t.target_id == s)
        });
        let selector = selector.filter(|_| !keyword).map(normal_selector);
        Self {
            all: selector.is_none(),
            selector,
            targets,
        }
    }

    /// `-t` as `display_argv` shows it: none for every client, the id for one, else the
    /// selector.
    fn flag(&self) -> Option<&str> {
        match self.targets.as_slice() {
            _ if self.all => None,
            [one] => Some(&one.target_id),
            _ => self.selector.as_deref(),
        }
    }

    fn validate(&self) -> std::result::Result<(), String> {
        if self.all != self.selector.is_none() {
            return Err("a scope is every client exactly when it has no selector".to_owned());
        }
        if self.selector.as_deref() == Some("") {
            return Err("a scope's selector is not empty".to_owned());
        }
        if self.targets.is_empty() {
            return Err("a scope holds at least one client".to_owned());
        }
        if self.targets.iter().any(|t| t.target_id.is_empty()) {
            return Err("every client of a scope has an id".to_owned());
        }
        if self
            .targets
            .windows(2)
            .any(|w| w[0].target_id > w[1].target_id)
        {
            return Err("a scope's clients are sorted by id".to_owned());
        }
        for (n, target) in self.targets.iter().enumerate() {
            if self.targets[..n].iter().any(|t| t.target == target.target) {
                return Err(format!("a scope names {} once", target.target));
            }
        }
        Ok(())
    }
}

/// `-t` as typed, a path among them made canonical.
fn normal_selector(selector: &str) -> String {
    let path = selector.contains('/') || selector == "." || selector == "..";
    if !path {
        return selector.to_owned();
    }
    std::fs::canonicalize(selector)
        .ok()
        .and_then(|p| p.into_os_string().into_string().ok())
        .unwrap_or_else(|| selector.to_owned())
}

/// What `undo` reverts: the transaction or run its preview resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum UndoSelection {
    Tx(TxId),
    Run(RunId),
}

// ---------------------------------------------------------------------------------------
// The variants.

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InstallIntent {
    pub refs: Vec<NormalizedRef>,
    /// The client `install` chose: `-t`, the default, or its probe's one client.
    pub target: TargetRef,
    pub target_id: String,
    pub channel: Option<Channel>,
    pub version: Option<String>,
    pub file: Option<AbsPath>,
    pub reinstall: bool,
    pub no_deps: bool,
    pub force: bool,
    pub strict: bool,
    pub refresh: bool,
    pub any_flavor: bool,
    pub no_any_flavor: bool,
    pub while_running: bool,
}

/// Bare `update` and `update --all` are one intent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateIntent {
    pub addons: Vec<String>,
    pub scope: TargetScope,
    pub channel: Option<Channel>,
    pub refresh: bool,
    pub strict: bool,
    pub no_deps: bool,
    pub while_running: bool,
}

/// `pin`'s and `unpin`'s (which takes no `version`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PinIntent {
    pub addon: String,
    pub scope: TargetScope,
    pub version: Option<String>,
    pub while_running: bool,
}

/// An adopting `adopt`: `--apply`, folder names or `--ref` (the report is not storable).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AdoptIntent {
    pub folders: Vec<String>,
    pub apply: bool,
    #[serde(rename = "ref")]
    pub r#ref: Option<NormalizedRef>,
    pub scope: TargetScope,
    pub no_verify: bool,
    pub refresh: bool,
    pub while_running: bool,
}

/// `sync` of the file its dry run read; `default_file` when no file was named.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SyncIntent {
    pub file: AbsPath,
    pub default_file: bool,
    pub scope: TargetScope,
    pub strict: bool,
    pub no_deps: bool,
    pub locked: bool,
    pub prune: bool,
    pub while_running: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CopyIntent {
    pub from: TargetRef,
    pub from_id: String,
    pub to: TargetRef,
    pub to_id: String,
    pub addons: Vec<String>,
    pub strict: bool,
    pub no_deps: bool,
    pub while_running: bool,
}

/// `remove` on the one client it settled on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RemoveIntent {
    pub addons: Vec<String>,
    pub target: TargetRef,
    pub target_id: String,
    pub while_running: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UndoIntent {
    pub selection: UndoSelection,
    pub saved_variables: Option<SvPolicy>,
    pub while_running: bool,
}

/// `backup restore` of a backup by its full id, on the client it belongs to. A restore
/// refuses while the client runs whatever `--while-running` says, so it records none.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RestoreIntent {
    pub backup: BackupId,
    pub target: TargetRef,
    pub target_id: String,
    pub addons: Vec<String>,
}

/// `backup prune` by full ids, or with `--keep` and the absolute cutoff `--older-than`
/// meant at the preview.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PruneIntent {
    pub ids: Vec<BackupId>,
    pub keep: Option<u64>,
    #[schemars(with = "Option<String>")]
    pub older_than: Option<Timestamp>,
    pub scope: TargetScope,
}

/// What a stored plan executes: exactly one variant per storable command (M4-D22), so a
/// plan file can name no other command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum StoredIntent {
    Install(InstallIntent),
    Update(UpdateIntent),
    Pin(PinIntent),
    Unpin(PinIntent),
    Adopt(AdoptIntent),
    Sync(SyncIntent),
    Copy(CopyIntent),
    Remove(RemoveIntent),
    Undo(UndoIntent),
    BackupRestore(RestoreIntent),
    BackupPrune(PruneIntent),
}

/// What a run resolved, handed to [`StoredIntent::from_args`] with its arguments: keys,
/// parsed refs and full ids are known only once the run resolved them, and the intent
/// must describe the plan its token describes.
pub enum StorableArgs<'a> {
    Install {
        args: &'a InstallArgs,
        target: IntentTarget,
        refs: Vec<NormalizedRef>,
        file: Option<AbsPath>,
    },
    Update {
        args: &'a UpdateArgs,
        scope: TargetScope,
    },
    Pin {
        args: &'a PinArgs,
        scope: TargetScope,
    },
    Unpin {
        args: &'a UnpinArgs,
        scope: TargetScope,
    },
    Adopt {
        args: &'a AdoptArgs,
        scope: TargetScope,
        r#ref: Option<NormalizedRef>,
    },
    Sync {
        args: &'a SyncArgs,
        scope: TargetScope,
        file: AbsPath,
        default_file: bool,
    },
    Copy {
        args: &'a CopyArgs,
        from: IntentTarget,
        to: IntentTarget,
    },
    Remove {
        args: &'a RemoveArgs,
        target: IntentTarget,
    },
    Undo {
        args: &'a UndoArgs,
        selection: UndoSelection,
    },
    BackupRestore {
        args: &'a BackupRestoreArgs,
        backup: BackupId,
        target: IntentTarget,
    },
    BackupPrune {
        args: &'a BackupPruneArgs,
        ids: Vec<BackupId>,
        older_than: Option<Timestamp>,
        scope: TargetScope,
    },
}

impl StoredIntent {
    /// The eleven storable command paths, with the widget's `destructive` flag.
    pub const PATHS: [(&'static str, bool); 11] = [
        ("install", false),
        ("update", false),
        ("pin", false),
        ("unpin", false),
        ("adopt", false),
        ("sync", false),
        ("copy", false),
        ("remove", true),
        ("undo", true),
        ("backup restore", true),
        ("backup prune", true),
    ];

    /// The canonical command path (`backup restore`); the serialized tag is never one.
    pub fn command_path(&self) -> &'static str {
        match self {
            Self::Install(_) => "install",
            Self::Update(_) => "update",
            Self::Pin(_) => "pin",
            Self::Unpin(_) => "unpin",
            Self::Adopt(_) => "adopt",
            Self::Sync(_) => "sync",
            Self::Copy(_) => "copy",
            Self::Remove(_) => "remove",
            Self::Undo(_) => "undo",
            Self::BackupRestore(_) => "backup restore",
            Self::BackupPrune(_) => "backup prune",
        }
    }

    pub fn destructive(&self) -> bool {
        matches!(
            self,
            Self::Remove(_) | Self::Undo(_) | Self::BackupRestore(_) | Self::BackupPrune(_)
        )
    }

    /// What the intent's options say its approval must flag; false where the variant
    /// has no such option.
    pub fn approval_flags(&self) -> ApprovalFlags {
        let (reinstall, force) = match self {
            Self::Install(i) => (i.reinstall, i.force),
            _ => (false, false),
        };
        let while_running = match self {
            Self::Install(i) => i.while_running,
            Self::Update(u) => u.while_running,
            Self::Pin(p) | Self::Unpin(p) => p.while_running,
            Self::Adopt(a) => a.while_running,
            Self::Sync(s) => s.while_running,
            Self::Copy(c) => c.while_running,
            Self::Remove(r) => r.while_running,
            Self::Undo(u) => u.while_running,
            Self::BackupRestore(_) | Self::BackupPrune(_) => false,
        };
        ApprovalFlags {
            reinstall,
            force,
            while_running,
            destructive: self.destructive(),
        }
    }

    /// The absolute inputs `apply` checks are still there: install's `--file`, a `file:`
    /// ref's path, the `sync` file.
    pub fn inputs(&self) -> Vec<&AbsPath> {
        match self {
            Self::Install(i) => i
                .refs
                .iter()
                .filter_map(NormalizedRef::file)
                .chain(i.file.as_ref())
                .collect(),
            Self::Sync(s) => vec![&s.file],
            Self::Update(_)
            | Self::Pin(_)
            | Self::Unpin(_)
            | Self::Adopt(_)
            | Self::Copy(_)
            | Self::Remove(_)
            | Self::Undo(_)
            | Self::BackupRestore(_)
            | Self::BackupPrune(_) => Vec::new(),
        }
    }

    /// The rules decoding cannot express; a failure names its rule.
    pub fn validate(&self) -> std::result::Result<(), String> {
        let names = |what: &str, names: &[String]| {
            if names.iter().any(String::is_empty) {
                Err(format!("{what} holds no empty name"))
            } else {
                Ok(())
            }
        };
        let id = |what: &str, id: &str| {
            if id.is_empty() {
                Err(format!("{what} has an id"))
            } else {
                Ok(())
            }
        };
        match self {
            Self::Install(i) => {
                if i.refs.is_empty() {
                    return Err("install names at least one ref".to_owned());
                }
                if i.file.is_some() && i.refs.len() != 1 {
                    return Err("install --file names exactly one ref".to_owned());
                }
                if i.any_flavor && i.no_any_flavor {
                    return Err(
                        "install takes --any-flavor or --no-any-flavor, not both".to_owned()
                    );
                }
                id("install's client", &i.target_id)
            }
            Self::Update(u) => {
                names("update's addons", &u.addons)?;
                u.scope.validate()
            }
            Self::Pin(p) | Self::Unpin(p) => {
                if p.addon.is_empty() {
                    return Err(format!("{} names an addon", self.command_path()));
                }
                if matches!(self, Self::Unpin(_)) && p.version.is_some() {
                    return Err("unpin takes no version".to_owned());
                }
                p.scope.validate()
            }
            Self::Adopt(a) => {
                if !a.apply && a.folders.is_empty() && a.r#ref.is_none() {
                    return Err("a stored adopt adopts: --apply, a folder or --ref".to_owned());
                }
                if a.apply && !a.folders.is_empty() {
                    return Err("adopt takes --apply or folder names, not both".to_owned());
                }
                names("adopt's folders", &a.folders)?;
                a.scope.validate()
            }
            Self::Sync(s) => s.scope.validate(),
            Self::Copy(c) => {
                if c.from == c.to {
                    return Err("copy's sides are two clients".to_owned());
                }
                id("copy's --from", &c.from_id)?;
                id("copy's --to", &c.to_id)?;
                names("copy's addons", &c.addons)
            }
            Self::Remove(r) => {
                if r.addons.is_empty() {
                    return Err("remove names at least one addon".to_owned());
                }
                names("remove's addons", &r.addons)?;
                id("remove's client", &r.target_id)
            }
            Self::Undo(_) => Ok(()),
            Self::BackupRestore(r) => {
                names("backup restore's addons", &r.addons)?;
                id("backup restore's client", &r.target_id)
            }
            Self::BackupPrune(p) => {
                if !p.ids.is_empty() && (p.keep.is_some() || p.older_than.is_some()) {
                    return Err("backup prune takes ids, or --keep and --older-than".to_owned());
                }
                p.scope.validate()
            }
        }
    }

    /// The intent as a person would type it, for display and history only. Positionals
    /// come after `--` at the end when one starts with `-`; it never holds `--dry-run`,
    /// `--if-match` or a presentation flag.
    pub fn display_argv(&self) -> Vec<String> {
        let mut f = Flags::default();
        let positionals: Vec<String> = match self {
            Self::Install(i) => {
                f.value("-t", Some(&i.target_id));
                f.value("--channel", i.channel.map(channel_word));
                f.value("--version", i.version.as_deref());
                f.value("--file", i.file.as_ref().map(AbsPath::as_str));
                f.switch("--reinstall", i.reinstall);
                f.switch("--no-deps", i.no_deps);
                f.switch("--force", i.force);
                f.switch("--strict", i.strict);
                f.switch("--refresh", i.refresh);
                f.switch("--any-flavor", i.any_flavor);
                f.switch("--no-any-flavor", i.no_any_flavor);
                f.switch("--while-running", i.while_running);
                i.refs.iter().map(ToString::to_string).collect()
            }
            Self::Update(u) => {
                f.value("-t", u.scope.flag());
                f.value("--channel", u.channel.map(channel_word));
                f.switch("--refresh", u.refresh);
                f.switch("--strict", u.strict);
                f.switch("--no-deps", u.no_deps);
                f.switch("--while-running", u.while_running);
                u.addons.clone()
            }
            Self::Pin(p) | Self::Unpin(p) => {
                f.value("-t", p.scope.flag());
                f.value("--version", p.version.as_deref());
                f.switch("--while-running", p.while_running);
                vec![p.addon.clone()]
            }
            Self::Adopt(a) => {
                f.switch("--apply", a.apply);
                let r#ref = a.r#ref.as_ref().map(ToString::to_string);
                f.value("--ref", r#ref.as_deref());
                f.value("-t", a.scope.flag());
                f.switch("--no-verify", a.no_verify);
                f.switch("--refresh", a.refresh);
                f.switch("--while-running", a.while_running);
                a.folders.clone()
            }
            Self::Sync(s) => {
                f.value("-t", s.scope.flag());
                f.switch("--strict", s.strict);
                f.switch("--no-deps", s.no_deps);
                f.switch("--locked", s.locked);
                f.switch("--prune", s.prune);
                f.switch("--while-running", s.while_running);
                if s.default_file {
                    Vec::new()
                } else {
                    vec![s.file.to_string()]
                }
            }
            Self::Copy(c) => {
                f.value("--from", Some(&c.from_id));
                f.value("--to", Some(&c.to_id));
                f.switch("--strict", c.strict);
                f.switch("--no-deps", c.no_deps);
                f.switch("--while-running", c.while_running);
                c.addons.clone()
            }
            Self::Remove(r) => {
                f.value("-t", Some(&r.target_id));
                f.switch("--while-running", r.while_running);
                r.addons.clone()
            }
            Self::Undo(u) => {
                let positional = match u.selection {
                    UndoSelection::Tx(tx) => vec![tx.to_string()],
                    UndoSelection::Run(run) => {
                        f.value("--run", Some(&run.to_string()));
                        Vec::new()
                    }
                };
                f.value("--saved-variables", u.saved_variables.map(policy_word));
                f.switch("--while-running", u.while_running);
                positional
            }
            Self::BackupRestore(r) => {
                f.value("-t", Some(&r.target_id));
                for addon in &r.addons {
                    f.value("--addon", Some(addon));
                }
                vec![r.backup.to_string()]
            }
            Self::BackupPrune(p) => {
                f.value("--keep", p.keep.map(|k| k.to_string()).as_deref());
                f.value(
                    "--older-than",
                    p.older_than.map(|t| t.to_string()).as_deref(),
                );
                f.value("-t", p.scope.flag());
                p.ids.iter().map(ToString::to_string).collect()
            }
        };
        let mut argv = vec!["omaddons".to_owned()];
        argv.extend(self.command_path().split(' ').map(str::to_owned));
        if positionals.iter().any(|p| p.starts_with('-')) {
            argv.extend(f.0);
            argv.push("--".to_owned());
            argv.extend(positionals);
        } else {
            argv.extend(positionals);
            argv.extend(f.0);
        }
        argv
    }

    /// The intent of a run's arguments and what it resolved; one that does not validate
    /// is a bug, never the user's.
    pub fn from_args(ctx: &Context, args: &StorableArgs<'_>) -> Result<Self> {
        let while_running = ctx.while_running;
        let intent = match args {
            StorableArgs::Install {
                args,
                target,
                refs,
                file,
            } => Self::Install(InstallIntent {
                refs: refs.clone(),
                target: target.target.clone(),
                target_id: target.target_id.clone(),
                channel: channel(args.channel.as_deref()),
                version: args.version.clone(),
                file: file.clone(),
                reinstall: args.reinstall,
                no_deps: args.no_deps,
                force: args.force,
                strict: args.strict,
                refresh: args.refresh,
                any_flavor: args.any_flavor,
                no_any_flavor: args.no_any_flavor,
                while_running,
            }),
            StorableArgs::Update { args, scope } => Self::Update(UpdateIntent {
                addons: args.addons.clone(),
                scope: scope.clone(),
                channel: channel(args.channel.as_deref()),
                refresh: args.refresh,
                strict: args.strict,
                no_deps: args.no_deps,
                while_running,
            }),
            StorableArgs::Pin { args, scope } => Self::Pin(PinIntent {
                addon: args.addon.clone(),
                scope: scope.clone(),
                version: args.version.clone(),
                while_running,
            }),
            StorableArgs::Unpin { args, scope } => Self::Unpin(PinIntent {
                addon: args.addon.clone(),
                scope: scope.clone(),
                version: None,
                while_running,
            }),
            StorableArgs::Adopt { args, scope, r#ref } => Self::Adopt(AdoptIntent {
                folders: args.folders.clone(),
                apply: args.apply,
                r#ref: r#ref.clone(),
                scope: scope.clone(),
                no_verify: args.no_verify,
                refresh: args.refresh,
                while_running,
            }),
            StorableArgs::Sync {
                args,
                scope,
                file,
                default_file,
            } => Self::Sync(SyncIntent {
                file: file.clone(),
                default_file: *default_file,
                scope: scope.clone(),
                strict: args.strict,
                no_deps: args.no_deps,
                locked: args.locked,
                prune: args.prune,
                while_running,
            }),
            StorableArgs::Copy { args, from, to } => Self::Copy(CopyIntent {
                from: from.target.clone(),
                from_id: from.target_id.clone(),
                to: to.target.clone(),
                to_id: to.target_id.clone(),
                addons: args.addons.clone(),
                strict: args.strict,
                no_deps: args.no_deps,
                while_running,
            }),
            StorableArgs::Remove { args, target } => Self::Remove(RemoveIntent {
                addons: args.addons.clone(),
                target: target.target.clone(),
                target_id: target.target_id.clone(),
                while_running,
            }),
            StorableArgs::Undo { args, selection } => Self::Undo(UndoIntent {
                selection: *selection,
                saved_variables: args.saved_variables.as_deref().map(policy),
                while_running,
            }),
            StorableArgs::BackupRestore {
                args,
                backup,
                target,
            } => Self::BackupRestore(RestoreIntent {
                backup: *backup,
                target: target.target.clone(),
                target_id: target.target_id.clone(),
                addons: args.addon.clone(),
            }),
            StorableArgs::BackupPrune {
                args,
                ids,
                older_than,
                scope,
            } => Self::BackupPrune(PruneIntent {
                ids: ids.clone(),
                keep: args.keep.map(|k| u64::try_from(k).unwrap_or(u64::MAX)),
                older_than: *older_than,
                scope: scope.clone(),
            }),
        };
        intent.validate().map_err(|rule| {
            Error::internal(format!("the preview's intent does not validate: {rule}"))
        })?;
        Ok(intent)
    }
}

// ---------------------------------------------------------------------------------------
// Back to arguments: what `apply` hands each command's own `run` (M4-D23).

impl StoredIntent {
    /// The `-t` an applied run gets: the resolved key (or provisional id) of a one-client
    /// intent; `None` when it covers every client, and for `copy` and `undo`, which take
    /// no `-t`; for a scope of several clients, the selector as typed, which the token's
    /// `targets` part binds to exactly the clients the preview selected.
    pub fn target_selector(&self) -> Option<String> {
        let scoped = |scope: &TargetScope| match scope.targets.as_slice() {
            _ if scope.all => None,
            [one] => Some(one.target.to_string()),
            _ => scope.selector.clone(),
        };
        match self {
            Self::Install(i) => Some(i.target.to_string()),
            Self::Remove(r) => Some(r.target.to_string()),
            Self::BackupRestore(r) => Some(r.target.to_string()),
            Self::Update(u) => scoped(&u.scope),
            Self::Pin(p) | Self::Unpin(p) => scoped(&p.scope),
            Self::Adopt(a) => scoped(&a.scope),
            Self::Sync(s) => scoped(&s.scope),
            Self::BackupPrune(p) => scoped(&p.scope),
            Self::Copy(_) | Self::Undo(_) => None,
        }
    }

    /// The running policy the preview was approved under (`--while-running`); a restore
    /// records none, since it refuses while the client runs whatever the flag says.
    pub fn while_running(&self) -> bool {
        self.approval_flags().while_running
    }
}

/// `install`'s arguments for `intent`. Defaulted: `strict` (it changes only the exit code
/// of a partial result, which `apply` reports through `partial` and its outcomes) and
/// `refresh` (`apply` revalidates every source). Not arguments: the client (`-t`) and
/// `--while-running`, which the applied run's context carries.
impl From<&InstallIntent> for InstallArgs {
    fn from(i: &InstallIntent) -> Self {
        Self {
            refs: i.refs.iter().map(ToString::to_string).collect(),
            channel: i.channel.map(|c| channel_word(c).to_owned()),
            version: i.version.clone(),
            file: i.file.as_ref().map(|f| f.as_path().to_path_buf()),
            reinstall: i.reinstall,
            no_deps: i.no_deps,
            force: i.force,
            strict: false,
            refresh: false,
            any_flavor: i.any_flavor,
            no_any_flavor: i.no_any_flavor,
        }
    }
}

/// `update`'s arguments for `intent`. Defaulted: `all` (bare `update` is `--all`, and
/// `--all` with names is a usage error), `strict` and `refresh` (as for `install`).
impl From<&UpdateIntent> for UpdateArgs {
    fn from(u: &UpdateIntent) -> Self {
        Self {
            addons: u.addons.clone(),
            all: false,
            channel: u.channel.map(|c| channel_word(c).to_owned()),
            refresh: false,
            strict: false,
            no_deps: u.no_deps,
        }
    }
}

/// `pin`'s arguments for `intent`; nothing is defaulted.
impl From<&PinIntent> for PinArgs {
    fn from(p: &PinIntent) -> Self {
        Self {
            addon: p.addon.clone(),
            version: p.version.clone(),
        }
    }
}

/// `unpin`'s arguments for `intent` (whose `version` is always `None`).
impl From<&PinIntent> for UnpinArgs {
    fn from(p: &PinIntent) -> Self {
        Self {
            addon: p.addon.clone(),
        }
    }
}

/// `adopt`'s arguments for `intent`. Defaulted: `refresh` (as for `install`).
impl From<&AdoptIntent> for AdoptArgs {
    fn from(a: &AdoptIntent) -> Self {
        Self {
            folders: a.folders.clone(),
            apply: a.apply,
            r#ref: a.r#ref.as_ref().map(ToString::to_string),
            no_verify: a.no_verify,
            refresh: false,
        }
    }
}

/// `sync`'s arguments for `intent`: the file it read, or none when it read the default
/// (the config directory's `addons.toml`, found again the same way). Defaulted: `strict`
/// (as for `install`).
impl From<&SyncIntent> for SyncArgs {
    fn from(s: &SyncIntent) -> Self {
        Self {
            file: (!s.default_file).then(|| s.file.as_path().to_path_buf()),
            strict: false,
            no_deps: s.no_deps,
            locked: s.locked,
            prune: s.prune,
        }
    }
}

/// `copy`'s arguments for `intent`, each side by its key (or provisional id). Defaulted:
/// `strict` (as for `install`).
impl From<&CopyIntent> for CopyArgs {
    fn from(c: &CopyIntent) -> Self {
        Self {
            from: c.from.to_string(),
            to: c.to.to_string(),
            addons: c.addons.clone(),
            strict: false,
            no_deps: c.no_deps,
        }
    }
}

/// `remove`'s arguments for `intent`; nothing is defaulted.
impl From<&RemoveIntent> for RemoveArgs {
    fn from(r: &RemoveIntent) -> Self {
        Self {
            addons: r.addons.clone(),
        }
    }
}

/// `undo`'s arguments for `intent`: the full transaction or run its preview resolved.
/// Defaulted: `last_by`, which the preview already resolved into that selection.
impl From<&UndoIntent> for UndoArgs {
    fn from(u: &UndoIntent) -> Self {
        let (tx, run) = match u.selection {
            UndoSelection::Tx(tx) => (Some(tx.to_string()), None),
            UndoSelection::Run(run) => (None, Some(run.to_string())),
        };
        Self {
            tx,
            last_by: None,
            run,
            saved_variables: u.saved_variables.map(|p| policy_word(p).to_owned()),
        }
    }
}

/// `backup restore`'s arguments for `intent`, the backup by its full id; nothing is
/// defaulted.
impl From<&RestoreIntent> for BackupRestoreArgs {
    fn from(r: &RestoreIntent) -> Self {
        Self {
            id: r.backup.to_string(),
            addon: r.addons.clone(),
        }
    }
}

/// `backup prune`'s arguments for `intent`: full ids, or `--keep` and the absolute
/// cutoff the preview meant (an RFC 3339 time, which `--older-than` takes); nothing is
/// defaulted.
impl From<&PruneIntent> for BackupPruneArgs {
    fn from(p: &PruneIntent) -> Self {
        Self {
            ids: p.ids.iter().map(ToString::to_string).collect(),
            keep: p.keep.map(|k| usize::try_from(k).unwrap_or(usize::MAX)),
            older_than: p.older_than.map(|t| t.to_string()),
        }
    }
}

/// `display_argv`'s flags, in the order they are added.
#[derive(Default)]
struct Flags(Vec<String>);

impl Flags {
    fn switch(&mut self, flag: &str, on: bool) {
        if on {
            self.0.push(flag.to_owned());
        }
    }

    /// `flag value`, or `flag=value` for a value clap would read as a flag.
    fn value(&mut self, flag: &str, value: Option<&str>) {
        match value {
            Some(v) if v.starts_with('-') => self.0.push(format!("{flag}={v}")),
            Some(v) => self.0.extend([flag.to_owned(), v.to_owned()]),
            None => {}
        }
    }
}

/// `--channel`'s value; clap admits only these three.
fn channel(word: Option<&str>) -> Option<Channel> {
    word.map(|c| match c {
        "beta" => Channel::Beta,
        "alpha" => Channel::Alpha,
        _ => Channel::Stable,
    })
}

const fn channel_word(channel: Channel) -> &'static str {
    match channel {
        Channel::Stable => "stable",
        Channel::Beta => "beta",
        Channel::Alpha => "alpha",
    }
}

/// `--saved-variables`' value; clap admits only these three.
fn policy(word: &str) -> SvPolicy {
    match word {
        "restore" => SvPolicy::Restore,
        "keep" => SvPolicy::Keep,
        _ => SvPolicy::Auto,
    }
}

const fn policy_word(policy: SvPolicy) -> &'static str {
    match policy {
        SvPolicy::Auto => "auto",
        SvPolicy::Restore => "restore",
        SvPolicy::Keep => "keep",
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;
    use serde_json::{Value, json};

    use super::*;
    use crate::app::backup::{BackupAction, BackupArgs};
    use crate::cli::{Cli, Command};
    use crate::model::Actor;

    const ERA: &str = "01JB8Q2VA00000000000000001";
    const RETAIL: &str = "01JB8Q2VA00000000000000002";
    const ANNIVERSARY: &str = "01JB8Q2VA00000000000000003";
    const PTR: &str = "01JB8Q2VA00000000000000004";
    const TX: &str = "01JB8Q2VA00000000000000010";
    const RUN: &str = "01JB8Q2VA00000000000000011";
    const BACKUP: &str = "01JB8Q2VA00000000000000012";
    const OLD: &str = "01JB8Q2VA00000000000000013";

    fn key(k: &str) -> TargetRef {
        TargetRef::Key(k.parse().unwrap())
    }

    fn client(id: &str, k: &str) -> IntentTarget {
        IntentTarget {
            target: key(k),
            target_id: id.to_owned(),
        }
    }

    fn strings(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| (*w).to_owned()).collect()
    }

    fn abs(path: &str) -> AbsPath {
        AbsPath::new(path.to_owned()).unwrap()
    }

    fn name(n: &str) -> NormalizedRef {
        NormalizedRef::Name(n.to_owned())
    }

    /// `-t <id>`, which selected that one client.
    fn one(id: &str, k: &str) -> TargetScope {
        TargetScope {
            all: false,
            selector: Some(id.to_owned()),
            targets: vec![client(id, k)],
        }
    }

    /// No `-t`: every client, era and retail.
    fn every() -> TargetScope {
        TargetScope {
            all: true,
            selector: None,
            targets: vec![client("era", ERA), client("retail", RETAIL)],
        }
    }

    fn ctx(while_running: bool) -> Context {
        Context {
            output: crate::output::OutputConfig::machine(),
            command: "install",
            actor: Actor::Agent,
            reason: None,
            argv: Vec::new(),
            offline: false,
            offline_by_env: false,
            dry_run: true,
            while_running,
            if_match: None,
            target: None,
            started: std::time::Instant::now(),
            progress: crate::output::ProgressSink::none(),
            notes: crate::output::RunNotes::default(),
            stdin_is_tty: false,
            force_revalidate: false,
            applying: None,
        }
    }

    struct Offered {
        preview: Option<Preview>,
        data: Value,
    }

    impl Outcome for Offered {
        type Data = Value;

        fn command(&self) -> &'static str {
            "remove"
        }
        fn data(&self) -> &Value {
            &self.data
        }
        fn render_human(&self, w: &mut dyn Write, style: &Style) -> io::Result<()> {
            writeln!(w, "{}", style.bold("Would remove Foo"))
        }
    }

    impl Storable for Offered {
        fn take_preview(&mut self) -> Option<Preview> {
            self.preview.take()
        }
        fn record_plan(&mut self, id: PlanId, expiry: Timestamp) {
            self.data["plan_id"] = json!(id);
            self.data["plan_expires_at"] = json!(expiry);
        }
    }

    fn offered(changes: bool) -> Offered {
        let precondition = super::super::precondition::Precondition::new(
            "remove",
            vec![(TargetRef::parse("retail"), Some(7))],
            json!(["Foo"]),
            json!({"generation": 7}),
        );
        Offered {
            data: json!({"plan_id": null, "plan_expires_at": null, "precondition": precondition.token()}),
            preview: Some(Preview {
                intent: StoredIntent::Remove(RemoveIntent {
                    addons: vec!["Foo".into()],
                    target: TargetRef::parse("retail"),
                    target_id: "retail".into(),
                    while_running: false,
                }),
                precondition,
                releases: Vec::new(),
                approval: Approval::nothing(&ApprovalFlags::default()),
                changes,
            }),
        }
    }

    fn offer_paths(root: &Path) -> Paths {
        Paths::with_roots(
            root,
            root.join("config"),
            root.join("state"),
            root.join("cache"),
        )
    }

    #[test]
    fn offer_stores_only_previews_that_change_something() {
        for (name, preview, stored) in [
            ("change", Some(true), true),
            ("noop", Some(false), false),
            ("real", None, false),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let paths = offer_paths(dir.path());
            let ctx = ctx(false);
            let mut outcome = offered(preview.unwrap_or(false));
            if preview.is_none() {
                outcome.preview = None;
            }
            let note = offer_with_paths(&ctx, &mut outcome, || Ok(paths.clone()));
            assert_eq!(
                outcome.data["plan_id"].is_string(),
                stored,
                "{name}: {:?}",
                outcome.data
            );
            assert!(ctx.notes.snapshot().0.is_empty(), "{name}: {:?}", ctx.notes);
            if stored {
                let id: PlanId = serde_json::from_value(outcome.data["plan_id"].clone()).unwrap();
                let plan = plans::load(&paths.plan(&id)).unwrap();
                assert_eq!(
                    plan.precondition.token, outcome.data["precondition"],
                    "{name}: {plan:?}"
                );
                assert_eq!(
                    plan.summary,
                    ["Would remove Foo"],
                    "plain human rendering, without ANSI or plan line"
                );
                assert_eq!(
                    plan.expires_at.duration_since(plan.created_at),
                    jiff::SignedDuration::from_hours(24)
                );
                assert!(note.unwrap().contains("stored "));
            } else {
                assert!(
                    !paths.plans_dir().exists(),
                    "{name}: no store should be opened"
                );
                assert_eq!(
                    note.as_deref(),
                    preview.map(|_| "plan store: nothing to store"),
                    "{name}"
                );
            }
        }
    }

    // -----------------------------------------------------------------------------------
    // Back to arguments (M4-D23): each test names every field of the Args, so a field
    // added to one fails to compile here until it is classified as copied or defaulted.

    #[test]
    fn install_intent_converts_to_its_args() {
        let intent = InstallIntent {
            refs: vec![name("dbm"), NormalizedRef::File(abs("/home/u/Foo.zip"))],
            target: key(RETAIL),
            target_id: "retail".into(),
            channel: Some(Channel::Beta),
            version: Some("1.2".into()),
            file: Some(abs("/home/u/dl/foo.zip")),
            reinstall: true,
            no_deps: true,
            force: true,
            strict: true,
            refresh: true,
            any_flavor: true,
            no_any_flavor: false,
            while_running: true,
        };
        let InstallArgs {
            refs,
            channel,
            version,
            file,
            reinstall,
            no_deps,
            force,
            strict,
            refresh,
            any_flavor,
            no_any_flavor,
        } = InstallArgs::from(&intent);
        assert_eq!(refs, ["dbm", "file:/home/u/Foo.zip"]);
        assert_eq!(channel.as_deref(), Some("beta"));
        assert_eq!(version.as_deref(), Some("1.2"));
        assert_eq!(file, Some(PathBuf::from("/home/u/dl/foo.zip")));
        assert_eq!(
            (reinstall, no_deps, force, any_flavor, no_any_flavor),
            (true, true, true, true, false),
            "copied"
        );
        assert_eq!((strict, refresh), (false, false), "defaulted");
    }

    #[test]
    fn update_intent_converts_to_its_args() {
        let intent = UpdateIntent {
            addons: strings(&["DBM", "Plater"]),
            scope: one("retail", RETAIL),
            channel: Some(Channel::Alpha),
            refresh: true,
            strict: true,
            no_deps: true,
            while_running: true,
        };
        let UpdateArgs {
            addons,
            all,
            channel,
            refresh,
            strict,
            no_deps,
        } = UpdateArgs::from(&intent);
        assert_eq!(addons, ["DBM", "Plater"]);
        assert_eq!(channel.as_deref(), Some("alpha"));
        assert!(no_deps, "copied");
        assert_eq!((all, refresh, strict), (false, false, false), "defaulted");
    }

    #[test]
    fn pin_intent_converts_to_its_args() {
        let intent = PinIntent {
            addon: "DBM".into(),
            scope: every(),
            version: Some("12.1.14".into()),
            while_running: true,
        };
        let PinArgs { addon, version } = PinArgs::from(&intent);
        assert_eq!(
            (addon.as_str(), version.as_deref()),
            ("DBM", Some("12.1.14"))
        );
    }

    #[test]
    fn unpin_intent_converts_to_its_args() {
        let intent = PinIntent {
            addon: "DBM".into(),
            scope: every(),
            version: None,
            while_running: false,
        };
        let UnpinArgs { addon } = UnpinArgs::from(&intent);
        assert_eq!(addon, "DBM");
    }

    #[test]
    fn adopt_intent_converts_to_its_args() {
        let intent = AdoptIntent {
            folders: strings(&["Bagnon"]),
            apply: false,
            r#ref: Some(NormalizedRef::Canonical("wowi:25068".into())),
            scope: one("retail", RETAIL),
            no_verify: true,
            refresh: true,
            while_running: true,
        };
        let AdoptArgs {
            folders,
            apply,
            r#ref,
            no_verify,
            refresh,
        } = AdoptArgs::from(&intent);
        assert_eq!(folders, ["Bagnon"]);
        assert_eq!(r#ref.as_deref(), Some("wowi:25068"));
        assert_eq!((apply, no_verify), (false, true), "copied");
        assert!(!refresh, "defaulted");
        let applying = AdoptArgs::from(&AdoptIntent {
            folders: Vec::new(),
            apply: true,
            r#ref: None,
            ..intent
        });
        assert!(applying.apply && applying.r#ref.is_none(), "{applying:?}");
    }

    #[test]
    fn sync_intent_converts_to_its_args() {
        let intent = SyncIntent {
            file: abs("/home/u/dotfiles/addons.toml"),
            default_file: false,
            scope: every(),
            strict: true,
            no_deps: true,
            locked: true,
            prune: true,
            while_running: true,
        };
        let SyncArgs {
            file,
            strict,
            no_deps,
            locked,
            prune,
        } = SyncArgs::from(&intent);
        assert_eq!(file, Some(PathBuf::from("/home/u/dotfiles/addons.toml")));
        assert_eq!((no_deps, locked, prune), (true, true, true), "copied");
        assert!(!strict, "defaulted");
        let default = SyncArgs::from(&SyncIntent {
            default_file: true,
            ..intent
        });
        assert_eq!(
            default.file, None,
            "the default file is found again, as a direct run finds it"
        );
    }

    #[test]
    fn copy_intent_converts_to_its_args() {
        let intent = CopyIntent {
            from: key(ERA),
            from_id: "era".into(),
            to: TargetRef::Provisional("anniversary".into()),
            to_id: "anniversary".into(),
            addons: strings(&["Questie"]),
            strict: true,
            no_deps: true,
            while_running: true,
        };
        let CopyArgs {
            from,
            to,
            addons,
            strict,
            no_deps,
        } = CopyArgs::from(&intent);
        assert_eq!(
            (from.as_str(), to.as_str()),
            (ERA, "anniversary"),
            "each side by its key, else its provisional id"
        );
        assert_eq!(addons, ["Questie"]);
        assert!(no_deps, "copied");
        assert!(!strict, "defaulted");
    }

    #[test]
    fn remove_intent_converts_to_its_args() {
        let intent = RemoveIntent {
            addons: strings(&["DBM", "BigWigs"]),
            target: key(RETAIL),
            target_id: "retail".into(),
            while_running: true,
        };
        let RemoveArgs { addons } = RemoveArgs::from(&intent);
        assert_eq!(addons, ["DBM", "BigWigs"]);
    }

    #[test]
    fn undo_intent_converts_to_its_args() {
        for (name, selection, want_tx, want_run) in [
            ("tx", UndoSelection::Tx(TX.parse().unwrap()), Some(TX), None),
            (
                "run",
                UndoSelection::Run(RUN.parse().unwrap()),
                None,
                Some(RUN),
            ),
        ] {
            let intent = UndoIntent {
                selection,
                saved_variables: Some(SvPolicy::Keep),
                while_running: true,
            };
            let UndoArgs {
                tx,
                last_by,
                run,
                saved_variables,
            } = UndoArgs::from(&intent);
            assert_eq!(
                (tx.as_deref(), run.as_deref()),
                (want_tx, want_run),
                "{name}: the full id the preview resolved"
            );
            assert_eq!(saved_variables.as_deref(), Some("keep"), "{name}");
            assert_eq!(last_by, None, "{name}: already resolved into the selection");
        }
    }

    #[test]
    fn backup_restore_intent_converts_to_its_args() {
        let intent = RestoreIntent {
            backup: BACKUP.parse().unwrap(),
            target: key(RETAIL),
            target_id: "retail".into(),
            addons: strings(&["WeakAuras"]),
        };
        let BackupRestoreArgs { id, addon } = BackupRestoreArgs::from(&intent);
        assert_eq!(id, BACKUP, "the full id");
        assert_eq!(addon, ["WeakAuras"]);
    }

    #[test]
    fn backup_prune_intent_converts_to_its_args() {
        let cutoff: Timestamp = "2026-10-01T08:00:00Z".parse().unwrap();
        let by_rule = PruneIntent {
            ids: Vec::new(),
            keep: Some(3),
            older_than: Some(cutoff),
            scope: one("retail", RETAIL),
        };
        let BackupPruneArgs {
            ids,
            keep,
            older_than,
        } = BackupPruneArgs::from(&by_rule);
        assert!(ids.is_empty());
        assert_eq!(keep, Some(3));
        assert_eq!(older_than.as_deref(), Some("2026-10-01T08:00:00Z"));
        assert_eq!(
            crate::app::history::parse_since(older_than.as_deref().unwrap(), Timestamp::now()),
            Some(cutoff),
            "--older-than reads the absolute cutoff back"
        );
        let by_id = BackupPruneArgs::from(&PruneIntent {
            ids: vec![BACKUP.parse().unwrap(), OLD.parse().unwrap()],
            keep: None,
            older_than: None,
            scope: every(),
        });
        assert_eq!(by_id.ids, [BACKUP, OLD]);
        assert_eq!((by_id.keep, by_id.older_than), (None, None));
    }

    /// A case's name, its intent, and what it gives: the command path, the target
    /// selector, the running policy and how many inputs.
    type AccessorCase = (
        &'static str,
        StoredIntent,
        &'static str,
        Option<&'static str>,
        bool,
        usize,
    );

    #[test]
    fn intent_accessors() {
        let several = TargetScope {
            all: false,
            selector: Some("mainline".into()),
            targets: vec![client("ptr", PTR), client("retail", RETAIL)],
        };
        let cases: Vec<AccessorCase> = vec![
            (
                "install",
                StoredIntent::Install(InstallIntent {
                    refs: vec![NormalizedRef::File(abs("/home/u/Foo"))],
                    target: key(RETAIL),
                    target_id: "retail".into(),
                    channel: None,
                    version: None,
                    file: Some(abs("/home/u/Foo.zip")),
                    reinstall: false,
                    no_deps: false,
                    force: false,
                    strict: false,
                    refresh: false,
                    any_flavor: false,
                    no_any_flavor: false,
                    while_running: true,
                }),
                "install",
                Some(RETAIL),
                true,
                2,
            ),
            (
                "update_every_client",
                StoredIntent::Update(UpdateIntent {
                    addons: Vec::new(),
                    scope: every(),
                    channel: None,
                    refresh: false,
                    strict: false,
                    no_deps: false,
                    while_running: false,
                }),
                "update",
                None,
                false,
                0,
            ),
            (
                "pin_one_client",
                StoredIntent::Pin(PinIntent {
                    addon: "DBM".into(),
                    scope: one("retail", RETAIL),
                    version: None,
                    while_running: false,
                }),
                "pin",
                Some(RETAIL),
                false,
                0,
            ),
            (
                "unpin_several_clients",
                StoredIntent::Unpin(PinIntent {
                    addon: "DBM".into(),
                    scope: several.clone(),
                    version: None,
                    while_running: true,
                }),
                "unpin",
                Some("mainline"),
                true,
                0,
            ),
            (
                "sync",
                StoredIntent::Sync(SyncIntent {
                    file: abs("/home/u/addons.toml"),
                    default_file: true,
                    scope: one("era", ERA),
                    strict: false,
                    no_deps: false,
                    locked: false,
                    prune: false,
                    while_running: false,
                }),
                "sync",
                Some(ERA),
                false,
                1,
            ),
            (
                "copy_takes_no_target",
                StoredIntent::Copy(CopyIntent {
                    from: key(ERA),
                    from_id: "era".into(),
                    to: key(ANNIVERSARY),
                    to_id: "anniversary".into(),
                    addons: Vec::new(),
                    strict: false,
                    no_deps: false,
                    while_running: true,
                }),
                "copy",
                None,
                true,
                0,
            ),
            (
                "remove_on_a_provisional_client",
                StoredIntent::Remove(RemoveIntent {
                    addons: strings(&["Foo"]),
                    target: TargetRef::Provisional("retail".into()),
                    target_id: "retail".into(),
                    while_running: false,
                }),
                "remove",
                Some("retail"),
                false,
                0,
            ),
            (
                "undo_takes_no_target",
                StoredIntent::Undo(UndoIntent {
                    selection: UndoSelection::Tx(TX.parse().unwrap()),
                    saved_variables: None,
                    while_running: true,
                }),
                "undo",
                None,
                true,
                0,
            ),
            (
                "backup_restore_never_while_running",
                StoredIntent::BackupRestore(RestoreIntent {
                    backup: BACKUP.parse().unwrap(),
                    target: key(RETAIL),
                    target_id: "retail".into(),
                    addons: Vec::new(),
                }),
                "backup restore",
                Some(RETAIL),
                false,
                0,
            ),
            (
                "backup_prune_several_clients",
                StoredIntent::BackupPrune(PruneIntent {
                    ids: Vec::new(),
                    keep: Some(1),
                    older_than: None,
                    scope: several,
                }),
                "backup prune",
                Some("mainline"),
                false,
                0,
            ),
        ];
        for (name, intent, command, target, while_running, inputs) in cases {
            assert_eq!(
                (
                    intent.command_path(),
                    intent.target_selector().as_deref(),
                    intent.while_running(),
                    intent.inputs().len()
                ),
                (command, target, while_running, inputs),
                "{name}: {}",
                serde_json::to_string(&intent).unwrap()
            );
        }
    }

    #[test]
    fn a_dry_run_while_applying_stores_nothing() {
        let approval = serde_json::to_value(Approval::nothing(&ApprovalFlags::default())).unwrap();
        let with_approval = || {
            let mut outcome = offered(true);
            outcome.data["approval"] = approval.clone();
            outcome
        };
        // An ordinary dry run of the same preview stores it.
        let dir = tempfile::tempdir().unwrap();
        let paths = offer_paths(dir.path());
        let mut ordinary = with_approval();
        offer_with_paths(&ctx(false), &mut ordinary, || Ok(paths.clone()));
        assert!(ordinary.data["plan_id"].is_string(), "{:?}", ordinary.data);

        let dir = tempfile::tempdir().unwrap();
        let paths = offer_paths(dir.path());
        let mut applying = ctx(false);
        applying.applying = Some(PlanId::new());
        let mut outcome = with_approval();
        let before = outcome.data.clone();
        let note = offer_with_paths(&applying, &mut outcome, || Ok(paths.clone()));
        assert_eq!(
            note.as_deref(),
            Some("plan store: nothing stored while applying a stored plan")
        );
        assert_eq!(
            outcome.data, before,
            "no plan_id, no expiry; the approval stands"
        );
        assert_eq!(outcome.data["approval"], ordinary.data["approval"]);
        assert!(outcome.preview.is_none(), "the preview was taken");
        assert!(
            !paths.plans_dir().exists(),
            "no plan file and no .lock: {:?}",
            std::fs::read_dir(dir.path()).map(|d| d.count())
        );
        assert!(
            applying.notes.snapshot().0.is_empty(),
            "no plan_not_stored warning: {:?}",
            applying.notes
        );
    }

    #[test]
    fn offer_warns_when_the_store_declines() {
        let dir = tempfile::tempdir().unwrap();
        let paths = offer_paths(dir.path());
        let first_ctx = ctx(false);
        let mut first = offered(true);
        offer_with_paths(&first_ctx, &mut first, || Ok(paths.clone()));
        let id: PlanId = serde_json::from_value(first.data["plan_id"].clone()).unwrap();
        let mut template = plans::load(&paths.plan(&id)).unwrap();
        // Different intent from the offered preview, so a full store cannot dedupe it.
        if let StoredIntent::Remove(intent) = &mut template.intent {
            intent.addons = vec!["Other".into()];
        }
        template.display_argv = template.intent.display_argv();
        std::fs::remove_file(paths.plan(&id)).unwrap();
        for _ in 0..crate::limits::PLANS_MAX_FILES {
            template.plan_id = PlanId::new();
            std::fs::write(
                paths.plan(&template.plan_id),
                plans::encode(&template).unwrap(),
            )
            .unwrap();
        }
        let ctx = ctx(false);
        let mut outcome = offered(true);
        let note = offer_with_paths(&ctx, &mut outcome, || Ok(paths.clone())).unwrap();
        assert!(outcome.data["plan_id"].is_null(), "{:?}", outcome.data);
        assert!(super::super::precondition::is_token(
            outcome.data["precondition"].as_str().unwrap()
        ));
        let warnings = ctx.notes.snapshot().0;
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert_eq!(warnings[0].code, WarningCode::PlanNotStored);
        assert!(
            warnings[0].message.contains("store is full"),
            "{warnings:?}"
        );
        assert!(note.contains("not stored (Full"), "{note}");
    }

    #[test]
    fn offer_io_failure_preserves_the_preview_and_token() {
        let dir = tempfile::tempdir().unwrap();
        let paths = offer_paths(dir.path());
        std::fs::create_dir_all(paths.cache_dir()).unwrap();
        std::fs::write(paths.plans_dir(), b"not a directory").unwrap();
        let ctx = ctx(false);
        let mut outcome = offered(true);
        let before = outcome.data.clone();
        let note = offer_with_paths(&ctx, &mut outcome, || Ok(paths)).unwrap();
        assert_eq!(outcome.data, before);
        let warnings = ctx.notes.snapshot().0;
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(
            warnings[0].message.contains("could not be stored"),
            "{warnings:?}"
        );
        assert!(note.contains("not stored (Io"), "{note}");
    }

    #[test]
    fn render_plan_line_rounds_the_time_left() {
        let now: Timestamp = "2026-10-27T12:00:00Z".parse().unwrap();
        let id = PlanId::new();
        for (name, seconds, expected) in [
            ("fresh", 86_400, "24 hours"),
            ("23h40m", 85_200, "24 hours"),
            ("22h20m", 80_400, "22 hours"),
            ("hour", 3_600, "1 hour"),
            ("59m", 3_540, "59 minutes"),
            ("30s", 30, "1 minute"),
        ] {
            let expiry = now
                .checked_add(jiff::SignedDuration::from_secs(seconds))
                .unwrap();
            let mut out = Vec::new();
            render_plan_line_at(&mut out, Some(&id), Some(&expiry), now).unwrap();
            assert_eq!(
                String::from_utf8(out).unwrap(),
                format!("Plan {id} stored for {expected}.\n"),
                "{name}"
            );
        }
        let mut out = Vec::new();
        render_plan_line_at(&mut out, None, None, now).unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn plan_not_stored_warns_with_a_prose_fix_for_each_cause() {
        for (name, why, expected) in [
            (
                "full",
                NotStored::Full {
                    plans: 512,
                    bytes: 4_096,
                },
                "store is full",
            ),
            (
                "large",
                NotStored::TooLarge { bytes: 300_000 },
                "larger than 256 KiB",
            ),
            ("busy", NotStored::Busy, "locked by another preview"),
            (
                "io",
                NotStored::Io("permission denied".into()),
                "permission denied",
            ),
        ] {
            let warning = plan_not_stored(&why);
            assert_eq!(warning.code, WarningCode::PlanNotStored, "{name}");
            assert!(warning.message.contains(expected), "{name}: {warning:?}");
            assert!(warning.fix.unwrap().argv().is_none(), "{name}: prose fix");
        }
    }

    /// A command's clap arguments, owned.
    enum Args {
        Install(InstallArgs),
        Update(UpdateArgs),
        Pin(PinArgs),
        Unpin(UnpinArgs),
        Adopt(AdoptArgs),
        Sync(SyncArgs),
        Copy(CopyArgs),
        Remove(RemoveArgs),
        Undo(UndoArgs),
        Restore(BackupRestoreArgs),
        Prune(BackupPruneArgs),
    }

    /// `args` with what `intent` says its run resolved, as the run hands them to
    /// `from_args`; `None` for another command's.
    fn storable<'a>(args: &'a Args, intent: &StoredIntent) -> Option<StorableArgs<'a>> {
        let of = |target: &TargetRef, id: &str| IntentTarget {
            target: target.clone(),
            target_id: id.to_owned(),
        };
        Some(match (args, intent) {
            (Args::Install(args), StoredIntent::Install(i)) => StorableArgs::Install {
                args,
                target: of(&i.target, &i.target_id),
                refs: i.refs.clone(),
                file: i.file.clone(),
            },
            (Args::Update(args), StoredIntent::Update(u)) => StorableArgs::Update {
                args,
                scope: u.scope.clone(),
            },
            (Args::Pin(args), StoredIntent::Pin(p)) => StorableArgs::Pin {
                args,
                scope: p.scope.clone(),
            },
            (Args::Unpin(args), StoredIntent::Unpin(p)) => StorableArgs::Unpin {
                args,
                scope: p.scope.clone(),
            },
            (Args::Adopt(args), StoredIntent::Adopt(a)) => StorableArgs::Adopt {
                args,
                scope: a.scope.clone(),
                r#ref: a.r#ref.clone(),
            },
            (Args::Sync(args), StoredIntent::Sync(s)) => StorableArgs::Sync {
                args,
                scope: s.scope.clone(),
                file: s.file.clone(),
                default_file: s.default_file,
            },
            (Args::Copy(args), StoredIntent::Copy(c)) => StorableArgs::Copy {
                args,
                from: of(&c.from, &c.from_id),
                to: of(&c.to, &c.to_id),
            },
            (Args::Remove(args), StoredIntent::Remove(r)) => StorableArgs::Remove {
                args,
                target: of(&r.target, &r.target_id),
            },
            (Args::Undo(args), StoredIntent::Undo(u)) => StorableArgs::Undo {
                args,
                selection: u.selection,
            },
            (Args::Restore(args), StoredIntent::BackupRestore(r)) => StorableArgs::BackupRestore {
                args,
                backup: r.backup,
                target: of(&r.target, &r.target_id),
            },
            (Args::Prune(args), StoredIntent::BackupPrune(p)) => StorableArgs::BackupPrune {
                args,
                ids: p.ids.clone(),
                older_than: p.older_than,
                scope: p.scope.clone(),
            },
            _ => return None,
        })
    }

    /// A parsed command line's arguments, when its command is storable.
    fn args_of(command: Command) -> Option<Args> {
        Some(match command {
            Command::Install(a) => Args::Install(a),
            Command::Update(a) => Args::Update(a),
            Command::Pin(a) => Args::Pin(a),
            Command::Unpin(a) => Args::Unpin(a),
            Command::Adopt(a) => Args::Adopt(a),
            Command::Sync(a) => Args::Sync(a),
            Command::Copy(a) => Args::Copy(a),
            Command::Remove(a) => Args::Remove(a),
            Command::Undo(a) => Args::Undo(a),
            Command::Backup(BackupArgs {
                action: Some(BackupAction::Restore(a)),
            }) => Args::Restore(a),
            Command::Backup(BackupArgs {
                action: Some(BackupAction::Prune(a)),
            }) => Args::Prune(a),
            _ => return None,
        })
    }

    /// One run of a storable command.
    struct Row {
        name: &'static str,
        args: Args,
        while_running: bool,
        /// What `from_args` builds from `args` and what the run resolved.
        intent: StoredIntent,
        display: &'static [&'static str],
    }

    fn rows() -> Vec<Row> {
        let install = |refs: Vec<NormalizedRef>, target: &str, id: &str| InstallIntent {
            refs,
            target: key(target),
            target_id: id.to_owned(),
            channel: None,
            version: None,
            file: None,
            reinstall: false,
            no_deps: false,
            force: false,
            strict: false,
            refresh: false,
            any_flavor: false,
            no_any_flavor: false,
            while_running: false,
        };
        vec![
            Row {
                name: "install (A.3)",
                args: Args::Install(InstallArgs {
                    refs: strings(&["weakauras", "dbm"]),
                    ..InstallArgs::default()
                }),
                while_running: false,
                intent: StoredIntent::Install(install(
                    vec![name("weakauras"), name("dbm")],
                    ERA,
                    "era",
                )),
                display: &["omaddons", "install", "weakauras", "dbm", "-t", "era"],
            },
            Row {
                name: "install with every option",
                args: Args::Install(InstallArgs {
                    refs: strings(&["gh:o/foo"]),
                    channel: Some("beta".to_owned()),
                    version: Some("v2".to_owned()),
                    file: Some("../dl/foo.zip".into()),
                    reinstall: true,
                    no_deps: true,
                    force: true,
                    strict: true,
                    refresh: true,
                    any_flavor: true,
                    no_any_flavor: false,
                }),
                while_running: true,
                intent: StoredIntent::Install(InstallIntent {
                    channel: Some(Channel::Beta),
                    version: Some("v2".to_owned()),
                    file: Some(abs("/w/dl/foo.zip")),
                    reinstall: true,
                    no_deps: true,
                    force: true,
                    strict: true,
                    refresh: true,
                    any_flavor: true,
                    while_running: true,
                    ..install(
                        vec![NormalizedRef::Canonical("gh:o/foo".to_owned())],
                        RETAIL,
                        "retail",
                    )
                }),
                display: &[
                    "omaddons",
                    "install",
                    "gh:o/foo",
                    "-t",
                    "retail",
                    "--channel",
                    "beta",
                    "--version",
                    "v2",
                    "--file",
                    "/w/dl/foo.zip",
                    "--reinstall",
                    "--no-deps",
                    "--force",
                    "--strict",
                    "--refresh",
                    "--any-flavor",
                    "--while-running",
                ],
            },
            Row {
                name: "update on one client",
                args: Args::Update(UpdateArgs {
                    addons: strings(&["DBM"]),
                    channel: Some("beta".to_owned()),
                    refresh: true,
                    strict: true,
                    no_deps: true,
                    all: false,
                }),
                while_running: false,
                intent: StoredIntent::Update(UpdateIntent {
                    addons: strings(&["DBM"]),
                    scope: one("retail", RETAIL),
                    channel: Some(Channel::Beta),
                    refresh: true,
                    strict: true,
                    no_deps: true,
                    while_running: false,
                }),
                display: &[
                    "omaddons",
                    "update",
                    "DBM",
                    "-t",
                    "retail",
                    "--channel",
                    "beta",
                    "--refresh",
                    "--strict",
                    "--no-deps",
                ],
            },
            Row {
                name: "update on several clients",
                args: Args::Update(UpdateArgs::default()),
                while_running: true,
                intent: StoredIntent::Update(UpdateIntent {
                    addons: Vec::new(),
                    scope: TargetScope {
                        all: false,
                        selector: Some("mainline".to_owned()),
                        targets: vec![client("ptr", PTR), client("retail", RETAIL)],
                    },
                    channel: None,
                    refresh: false,
                    strict: false,
                    no_deps: false,
                    while_running: true,
                }),
                display: &["omaddons", "update", "-t", "mainline", "--while-running"],
            },
            Row {
                name: "pin --version",
                args: Args::Pin(PinArgs {
                    addon: "Foo".to_owned(),
                    version: Some("v1".to_owned()),
                }),
                while_running: false,
                intent: StoredIntent::Pin(PinIntent {
                    addon: "Foo".to_owned(),
                    scope: every(),
                    version: Some("v1".to_owned()),
                    while_running: false,
                }),
                display: &["omaddons", "pin", "Foo", "--version", "v1"],
            },
            Row {
                name: "unpin",
                args: Args::Unpin(UnpinArgs {
                    addon: "Foo".to_owned(),
                }),
                while_running: true,
                intent: StoredIntent::Unpin(PinIntent {
                    addon: "Foo".to_owned(),
                    scope: one("era", ERA),
                    version: None,
                    while_running: true,
                }),
                display: &["omaddons", "unpin", "Foo", "-t", "era", "--while-running"],
            },
            Row {
                name: "adopt a folder from a ref",
                args: Args::Adopt(AdoptArgs {
                    folders: strings(&["Foo"]),
                    r#ref: Some("gh:o/foo".to_owned()),
                    no_verify: true,
                    ..AdoptArgs::default()
                }),
                while_running: false,
                intent: StoredIntent::Adopt(AdoptIntent {
                    folders: strings(&["Foo"]),
                    apply: false,
                    r#ref: Some(NormalizedRef::Canonical("gh:o/foo".to_owned())),
                    scope: one("retail", RETAIL),
                    no_verify: true,
                    refresh: false,
                    while_running: false,
                }),
                display: &[
                    "omaddons",
                    "adopt",
                    "Foo",
                    "--ref",
                    "gh:o/foo",
                    "-t",
                    "retail",
                    "--no-verify",
                ],
            },
            Row {
                name: "adopt --apply",
                args: Args::Adopt(AdoptArgs {
                    apply: true,
                    refresh: true,
                    ..AdoptArgs::default()
                }),
                while_running: false,
                intent: StoredIntent::Adopt(AdoptIntent {
                    folders: Vec::new(),
                    apply: true,
                    r#ref: None,
                    scope: every(),
                    no_verify: false,
                    refresh: true,
                    while_running: false,
                }),
                display: &["omaddons", "adopt", "--apply", "--refresh"],
            },
            Row {
                name: "sync of the default file",
                args: Args::Sync(SyncArgs {
                    file: None,
                    strict: true,
                    no_deps: false,
                    locked: true,
                    prune: true,
                }),
                while_running: false,
                intent: StoredIntent::Sync(SyncIntent {
                    file: abs("/c/omaddons/addons.toml"),
                    default_file: true,
                    scope: every(),
                    strict: true,
                    no_deps: false,
                    locked: true,
                    prune: true,
                    while_running: false,
                }),
                display: &["omaddons", "sync", "--strict", "--locked", "--prune"],
            },
            Row {
                name: "sync of a named file",
                args: Args::Sync(SyncArgs {
                    file: Some("addons.toml".into()),
                    no_deps: true,
                    ..SyncArgs::default()
                }),
                while_running: false,
                intent: StoredIntent::Sync(SyncIntent {
                    file: abs("/w/addons.toml"),
                    default_file: false,
                    scope: one("retail", RETAIL),
                    strict: false,
                    no_deps: true,
                    locked: false,
                    prune: false,
                    while_running: false,
                }),
                display: &[
                    "omaddons",
                    "sync",
                    "/w/addons.toml",
                    "-t",
                    "retail",
                    "--no-deps",
                ],
            },
            Row {
                name: "copy",
                args: Args::Copy(CopyArgs {
                    from: "era".to_owned(),
                    to: "anniversary".to_owned(),
                    addons: strings(&["Questie"]),
                    strict: true,
                    no_deps: true,
                }),
                while_running: true,
                intent: StoredIntent::Copy(CopyIntent {
                    from: key(ERA),
                    from_id: "era".to_owned(),
                    to: key(ANNIVERSARY),
                    to_id: "anniversary".to_owned(),
                    addons: strings(&["Questie"]),
                    strict: true,
                    no_deps: true,
                    while_running: true,
                }),
                display: &[
                    "omaddons",
                    "copy",
                    "Questie",
                    "--from",
                    "era",
                    "--to",
                    "anniversary",
                    "--strict",
                    "--no-deps",
                    "--while-running",
                ],
            },
            Row {
                name: "remove",
                args: Args::Remove(RemoveArgs {
                    addons: strings(&["Foo", "Bar"]),
                }),
                while_running: false,
                intent: StoredIntent::Remove(RemoveIntent {
                    addons: strings(&["Foo", "Bar"]),
                    target: key(RETAIL),
                    target_id: "retail".to_owned(),
                    while_running: false,
                }),
                display: &["omaddons", "remove", "Foo", "Bar", "-t", "retail"],
            },
            Row {
                name: "remove of a name like a flag",
                args: Args::Remove(RemoveArgs {
                    addons: strings(&["-weird"]),
                }),
                while_running: false,
                intent: StoredIntent::Remove(RemoveIntent {
                    addons: strings(&["-weird"]),
                    target: key(RETAIL),
                    target_id: "retail".to_owned(),
                    while_running: false,
                }),
                display: &["omaddons", "remove", "-t", "retail", "--", "-weird"],
            },
            Row {
                name: "undo of a transaction by its prefix",
                args: Args::Undo(UndoArgs {
                    tx: Some("01jb8q2va0".to_owned()),
                    saved_variables: Some("keep".to_owned()),
                    ..UndoArgs::default()
                }),
                while_running: false,
                intent: StoredIntent::Undo(UndoIntent {
                    selection: UndoSelection::Tx(TX.parse().unwrap()),
                    saved_variables: Some(SvPolicy::Keep),
                    while_running: false,
                }),
                display: &["omaddons", "undo", TX, "--saved-variables", "keep"],
            },
            Row {
                name: "undo of the last agent run",
                args: Args::Undo(UndoArgs {
                    last_by: Some("agent".to_owned()),
                    ..UndoArgs::default()
                }),
                while_running: true,
                intent: StoredIntent::Undo(UndoIntent {
                    selection: UndoSelection::Run(RUN.parse().unwrap()),
                    saved_variables: None,
                    while_running: true,
                }),
                display: &["omaddons", "undo", "--run", RUN, "--while-running"],
            },
            Row {
                name: "backup restore of one addon",
                args: Args::Restore(BackupRestoreArgs {
                    id: "01JB8Q2VA0".to_owned(),
                    addon: strings(&["WeakAuras"]),
                }),
                while_running: false,
                intent: StoredIntent::BackupRestore(RestoreIntent {
                    backup: BACKUP.parse().unwrap(),
                    target: key(RETAIL),
                    target_id: "retail".to_owned(),
                    addons: strings(&["WeakAuras"]),
                }),
                display: &[
                    "omaddons",
                    "backup",
                    "restore",
                    BACKUP,
                    "-t",
                    "retail",
                    "--addon",
                    "WeakAuras",
                ],
            },
            Row {
                name: "backup prune by age",
                args: Args::Prune(BackupPruneArgs {
                    ids: Vec::new(),
                    keep: Some(3),
                    older_than: Some("30d".to_owned()),
                }),
                while_running: false,
                intent: StoredIntent::BackupPrune(PruneIntent {
                    ids: Vec::new(),
                    keep: Some(3),
                    older_than: Some("2026-09-24T12:00:00Z".parse().unwrap()),
                    scope: one("retail", RETAIL),
                }),
                display: &[
                    "omaddons",
                    "backup",
                    "prune",
                    "--keep",
                    "3",
                    "--older-than",
                    "2026-09-24T12:00:00Z",
                    "-t",
                    "retail",
                ],
            },
            Row {
                name: "backup prune by id",
                args: Args::Prune(BackupPruneArgs {
                    ids: strings(&["01JB8Q2VA0"]),
                    ..BackupPruneArgs::default()
                }),
                while_running: false,
                intent: StoredIntent::BackupPrune(PruneIntent {
                    ids: vec![OLD.parse().unwrap()],
                    keep: None,
                    older_than: None,
                    scope: one("retail", RETAIL),
                }),
                display: &["omaddons", "backup", "prune", OLD, "-t", "retail"],
            },
        ]
    }

    fn row(name: &str) -> StoredIntent {
        let found = rows().into_iter().find(|r| r.name == name);
        assert!(found.is_some(), "no row {name}");
        found.map(|r| r.intent).unwrap()
    }

    #[test]
    fn from_args_builds_each_variant() {
        for r in rows() {
            let args = storable(&r.args, &r.intent);
            assert!(
                args.is_some(),
                "{}: the row's args are another command's",
                r.name
            );
            let got = StoredIntent::from_args(&ctx(r.while_running), &args.unwrap());
            assert_eq!(
                got.as_ref().ok(),
                Some(&r.intent),
                "{}: from_args gave {got:?}",
                r.name
            );
        }
    }

    #[test]
    fn from_args_refuses_a_resolution_that_does_not_validate() {
        let args = InstallArgs {
            refs: strings(&["a", "b"]),
            ..InstallArgs::default()
        };
        let got = StoredIntent::from_args(
            &ctx(false),
            &StorableArgs::Install {
                args: &args,
                target: client("retail", RETAIL),
                refs: vec![name("a"), name("b")],
                file: Some(abs("/w/a.zip")),
            },
        );
        assert_eq!(
            got.as_ref().err().map(|e| e.code),
            Some(crate::error::ErrorCode::Internal),
            "{got:?}"
        );
    }

    #[test]
    fn intents_round_trip_through_json() {
        for r in rows() {
            let text = serde_json::to_string(&r.intent).unwrap();
            let back: std::result::Result<StoredIntent, _> = serde_json::from_str(&text);
            assert_eq!(back.as_ref().ok(), Some(&r.intent), "{}: {text}", r.name);
            assert_eq!(r.intent.validate(), Ok(()), "{}", r.name);
        }
        let install = row("install (A.3)");
        assert_eq!(
            serde_json::to_value(&install).unwrap(),
            json!({
                "command": "install", "refs": ["weakauras", "dbm"], "target": ERA,
                "target_id": "era", "channel": null, "version": null, "file": null,
                "reinstall": false, "no_deps": false, "force": false, "strict": false,
                "refresh": false, "any_flavor": false, "no_any_flavor": false,
                "while_running": false
            })
        );
        let text = serde_json::to_string(&install).unwrap();
        let head = format!(r#"{{"command":"install","refs":["weakauras","dbm"],"target":"{ERA}","#);
        assert!(text.starts_with(&head), "the tag first: {text}");
    }

    /// An edit of a stored intent's JSON.
    type Edit = fn(&mut Value);

    #[test]
    fn decoding_rejects_hand_edits() {
        let edits: Vec<(&str, &str, Edit)> = vec![
            ("unknown variant auth", "install (A.3)", |v| {
                v["command"] = json!("auth");
            }),
            ("unknown variant apply", "install (A.3)", |v| {
                v["command"] = json!("apply");
            }),
            ("relative file", "install with every option", |v| {
                v["file"] = json!("foo.zip");
            }),
            ("dotdot file", "install with every option", |v| {
                v["file"] = json!("/a/../b.zip");
            }),
            ("trailing slash", "install with every option", |v| {
                v["file"] = json!("/a/b/");
            }),
            ("dot component", "install with every option", |v| {
                v["file"] = json!("/a/./b.zip");
            }),
            ("relative file: ref", "install (A.3)", |v| {
                v["refs"] = json!(["file:foo.zip"]);
            }),
            ("bare a/b ref", "install (A.3)", |v| {
                v["refs"] = json!(["a/b"]);
            }),
            ("./x ref", "install (A.3)", |v| v["refs"] = json!(["./x"])),
            ("tilde-relative ref", "install (A.3)", |v| {
                v["refs"] = json!(["~/x.zip"]);
            }),
            ("-x ref", "install (A.3)", |v| v["refs"] = json!(["-x"])),
            ("malformed gh ref", "install (A.3)", |v| {
                v["refs"] = json!(["gh:owner"]);
            }),
            ("padded ref", "install (A.3)", |v| {
                v["refs"] = json!([" dbm"])
            }),
            ("install with no ref", "install (A.3)", |v| {
                v["refs"] = json!([])
            }),
            (
                "install file with two refs",
                "install with every option",
                |v| {
                    v["refs"] = json!(["gh:o/foo", "dbm"]);
                },
            ),
            (
                "install --any-flavor and --no-any-flavor",
                "install (A.3)",
                |v| {
                    v["any_flavor"] = json!(true);
                    v["no_any_flavor"] = json!(true);
                },
            ),
            ("unpin with version", "unpin", |v| {
                v["version"] = json!("v1")
            }),
            (
                "adopt that adopts nothing",
                "adopt a folder from a ref",
                |v| {
                    v["folders"] = json!([]);
                    v["ref"] = Value::Null;
                },
            ),
            ("adopt --apply with folders", "adopt --apply", |v| {
                v["folders"] = json!(["Foo"]);
            }),
            ("scope all with a selector", "pin --version", |v| {
                v["scope"]["selector"] = json!("retail");
            }),
            ("scope with no targets", "pin --version", |v| {
                v["scope"]["targets"] = json!([]);
            }),
            ("unsorted scope targets", "pin --version", |v| {
                let targets = v["scope"]["targets"].as_array_mut().unwrap();
                targets.reverse();
            }),
            ("duplicate scope target", "pin --version", |v| {
                let first = v["scope"]["targets"][0].clone();
                v["scope"]["targets"].as_array_mut().unwrap().push(first);
            }),
            ("unknown field in a scope", "pin --version", |v| {
                v["scope"]["x"] = json!(1);
            }),
            ("unknown field in a scope's client", "pin --version", |v| {
                v["scope"]["targets"][0]["x"] = json!(1);
            }),
            ("prune ids with keep", "backup prune by id", |v| {
                v["keep"] = json!(2);
            }),
            ("copy from == to", "copy", |v| v["to"] = v["from"].clone()),
            ("empty addon name", "remove", |v| {
                v["addons"] = json!(["Foo", ""]);
            }),
            ("remove of nothing", "remove", |v| v["addons"] = json!([])),
            (
                "undo of both a transaction and a run",
                "undo of the last agent run",
                |v| {
                    v["selection"]["tx"] = json!(TX);
                },
            ),
            (
                "a backup id that is no id",
                "backup restore of one addon",
                |v| {
                    v["backup"] = json!("01JB8Q2VA0");
                },
            ),
        ];
        let rejected = |v: &Value| match serde_json::from_value::<StoredIntent>(v.clone()) {
            Err(_) => true,
            Ok(intent) => intent.validate().is_err(),
        };
        for (edit, base, apply) in edits {
            let mut v = serde_json::to_value(row(base)).unwrap();
            apply(&mut v);
            assert!(rejected(&v), "{edit}: decoded {v} but must be rejected");
        }
        // Each variant struct carries its own `deny_unknown_fields`: the enum's tag does
        // not reach them.
        for r in rows() {
            let mut v = serde_json::to_value(&r.intent).unwrap();
            v["yes"] = json!(true);
            let error =
                serde_json::from_value::<StoredIntent>(v.clone()).map_err(|e| e.to_string());
            assert!(
                error
                    .as_ref()
                    .is_err_and(|e| e.contains("unknown field `yes`")),
                "unknown field in {}: decoded {v} but must be rejected for it: {error:?}",
                r.name
            );
        }
    }

    #[test]
    fn normalized_refs_from_each_match_kind() {
        let gh = |owner: &str, repo: &str| Ref::GitHub {
            owner: owner.to_owned(),
            repo: repo.to_owned(),
        };
        let file = |path: &str| Ref::File { path: path.into() };
        let cases = [
            (
                "path ./Foo.zip",
                "./Foo.zip",
                file("/w/Foo.zip"),
                MatchKind::Path,
                "file:/w/Foo.zip",
            ),
            (
                "canonical file:./Foo",
                "file:./Foo",
                file("/w/Foo"),
                MatchKind::Canonical,
                "file:/w/Foo",
            ),
            (
                "shorthand o/r",
                "o/r",
                gh("o", "r"),
                MatchKind::Shorthand,
                "gh:o/r",
            ),
            (
                "url",
                "https://github.com/o/r/releases/tag/v1",
                gh("o", "r"),
                MatchKind::Url,
                "https://github.com/o/r/releases/tag/v1",
            ),
            (
                "canonical gh:o/r",
                " gh:o/r ",
                gh("o", "r"),
                MatchKind::Canonical,
                "gh:o/r",
            ),
            (
                "canonical git:",
                "git:https://codeberg.org/a/b#main",
                Ref::Git {
                    url: "https://codeberg.org/a/b".to_owned(),
                    rev: Some("main".to_owned()),
                },
                MatchKind::Canonical,
                "git:https://codeberg.org/a/b#main",
            ),
            (
                "catalog wa",
                "wa",
                gh("WeakAuras", "WeakAuras2"),
                MatchKind::Catalog,
                "wa",
            ),
            (
                "installed DBM",
                "DBM",
                gh("DeadlyBossMods", "DeadlyBossMods"),
                MatchKind::Installed,
                "DBM",
            ),
        ];
        for (case, request, r#ref, matched_by, expected) in cases {
            let parsed = ParsedRef {
                r#ref,
                version_hint: None,
                asset_hint: None,
                matched_by,
                catalog_variants: None,
            };
            let got = NormalizedRef::from_parsed(request, &parsed);
            assert_eq!(
                got.as_ref().map(ToString::to_string),
                Ok(expected.to_owned()),
                "{case}: {got:?}"
            );
            let decoded: std::result::Result<NormalizedRef, _> =
                serde_json::from_value(json!(expected));
            assert_eq!(decoded.ok(), got.ok(), "{case}: decodes as stored");
        }
    }

    #[test]
    fn abs_path_resolves_and_rejects() {
        let dir = tempfile::tempdir().unwrap();
        let w = std::fs::canonicalize(dir.path()).unwrap();
        std::fs::create_dir(w.join("x")).unwrap();
        std::fs::write(w.join("y.zip"), b"zip").unwrap();
        let y = w.join("y.zip").display().to_string();
        for input in ["x/../y.zip", "./y.zip", "y.zip"] {
            let got = AbsPath::resolve(&w, Path::new(input));
            assert_eq!(
                got.as_ref().map(AbsPath::as_str),
                Ok(y.as_str()),
                "{input}: {got:?}"
            );
        }
        assert_eq!(
            AbsPath::resolve(&w, Path::new("/abs/q.zip")).map(String::from),
            Ok("/abs/q.zip".to_owned())
        );
        assert!(
            AbsPath::resolve(&w, Path::new("missing/../y.zip")).is_err(),
            "a `..` through a missing directory cannot be resolved"
        );
        for text in [
            "foo.zip", "/a/../b", "/a/b/", "/a/./b", "", "//a", "/a//b", "/a\0b",
        ] {
            assert!(
                AbsPath::new(text.to_owned()).is_err(),
                "{text:?} must be rejected"
            );
        }
    }

    #[test]
    fn update_all_and_bare_update_are_one_intent() {
        let scope = every();
        let all = UpdateArgs {
            all: true,
            ..UpdateArgs::default()
        };
        let bare = UpdateArgs::default();
        let intent = |args: &UpdateArgs| {
            StoredIntent::from_args(
                &ctx(false),
                &StorableArgs::Update {
                    args,
                    scope: scope.clone(),
                },
            )
            .ok()
        };
        assert_eq!(intent(&all), intent(&bare));
        assert_eq!(
            intent(&bare).map(|i| i.display_argv()),
            Some(strings(&["omaddons", "update"]))
        );
    }

    /// `intent` with `--while-running` flipped, for the variants that record it.
    fn flipped(intent: &StoredIntent) -> Option<StoredIntent> {
        let mut flipped = intent.clone();
        let flag = match &mut flipped {
            StoredIntent::Install(i) => &mut i.while_running,
            StoredIntent::Update(u) => &mut u.while_running,
            StoredIntent::Pin(p) | StoredIntent::Unpin(p) => &mut p.while_running,
            StoredIntent::Adopt(a) => &mut a.while_running,
            StoredIntent::Sync(s) => &mut s.while_running,
            StoredIntent::Copy(c) => &mut c.while_running,
            StoredIntent::Remove(r) => &mut r.while_running,
            StoredIntent::Undo(u) => &mut u.while_running,
            StoredIntent::BackupRestore(_) | StoredIntent::BackupPrune(_) => return None,
        };
        *flag = !*flag;
        Some(flipped)
    }

    #[test]
    fn display_argv_examples() {
        for r in rows() {
            let display = strings(r.display);
            assert_eq!(r.intent.display_argv(), display, "{}", r.name);
            let Some(other) = flipped(&r.intent) else {
                continue;
            };
            // `--while-running` is the last flag: before `--`, else at the end.
            let mut expected = display.clone();
            if let Some(at) = expected.iter().position(|w| w == "--while-running") {
                expected.remove(at);
            } else {
                let at = expected
                    .iter()
                    .position(|w| w == "--")
                    .unwrap_or(expected.len());
                expected.insert(at, "--while-running".to_owned());
            }
            assert_eq!(
                other.display_argv(),
                expected,
                "{}, --while-running flipped",
                r.name
            );
        }
    }

    #[test]
    fn display_argv_parses_back_to_the_same_intent() {
        for r in rows() {
            let argv = r.intent.display_argv();
            let cli = Cli::try_parse_from(&argv).map_err(|e| e.to_string());
            assert!(cli.is_ok(), "{}: {argv:?} does not parse: {cli:?}", r.name);
            let Ok(Cli { global, command }) = cli else {
                continue;
            };
            assert_eq!(
                crate::app::command_name(command.as_ref()),
                r.intent.command_path(),
                "{}: {argv:?}",
                r.name
            );
            let args = command.and_then(args_of);
            let rebuilt = args.as_ref().and_then(|args| {
                let storable = storable(args, &r.intent)?;
                StoredIntent::from_args(&ctx(global.while_running), &storable).ok()
            });
            assert_eq!(
                rebuilt.as_ref(),
                Some(&r.intent),
                "{}: {argv:?} gave {rebuilt:?}, the row is {:?}",
                r.name,
                r.intent
            );
        }
    }

    #[test]
    fn destructive_flags_and_the_eleven_paths() {
        let paths: Vec<&str> = StoredIntent::PATHS.iter().map(|(p, _)| *p).collect();
        assert_eq!(
            paths,
            [
                "install",
                "update",
                "pin",
                "unpin",
                "adopt",
                "sync",
                "copy",
                "remove",
                "undo",
                "backup restore",
                "backup prune"
            ]
        );
        let mut covered: Vec<&str> = Vec::new();
        for r in rows() {
            let path = r.intent.command_path();
            let flagged = StoredIntent::PATHS.iter().find(|(p, _)| *p == path);
            assert_eq!(
                flagged.map(|(_, d)| *d),
                Some(r.intent.destructive()),
                "{}",
                r.name
            );
            assert_eq!(
                r.intent.destructive(),
                matches!(path, "remove" | "undo" | "backup restore" | "backup prune"),
                "{}",
                r.name
            );
            if !covered.contains(&path) {
                covered.push(path);
            }
        }
        covered.sort_unstable();
        let mut all = paths.clone();
        all.sort_unstable();
        assert_eq!(covered, all, "every storable command has a row");
    }

    #[test]
    fn approval_flags_follow_the_options() {
        for r in rows() {
            let flags = r.intent.approval_flags();
            let expected = ApprovalFlags {
                reinstall: r.name == "install with every option",
                force: r.name == "install with every option",
                // A restore and a prune record none: they never take `--while-running`.
                while_running: r.while_running
                    && !matches!(
                        r.intent,
                        StoredIntent::BackupRestore(_) | StoredIntent::BackupPrune(_)
                    ),
                destructive: r.intent.destructive(),
            };
            assert_eq!(flags, expected, "{}", r.name);
        }
    }

    #[test]
    fn inputs_are_the_local_files() {
        let mut install = row("install with every option");
        if let StoredIntent::Install(i) = &mut install {
            i.refs = vec![NormalizedRef::File(abs("/w/Foo"))];
        }
        let inputs: Vec<&str> = install.inputs().into_iter().map(AbsPath::as_str).collect();
        assert_eq!(inputs, ["/w/Foo", "/w/dl/foo.zip"]);
        let sync = row("sync of a named file");
        let inputs: Vec<&str> = sync.inputs().into_iter().map(AbsPath::as_str).collect();
        assert_eq!(inputs, ["/w/addons.toml"]);
        assert!(row("remove").inputs().is_empty());
    }

    fn target(id: &str, k: Option<&str>) -> Target {
        Target {
            key: k.map(|k| k.parse().unwrap()),
            id: id.to_owned(),
            install_root: "/games/wow".into(),
            client_dir: format!("/games/wow/_{id}_").into(),
            flavor_dir: Some(format!("_{id}_")),
            product: None,
            install_kind: crate::rules::InstallKind::Battlenet,
            family: crate::rules::Family::Mainline,
            kind: crate::rules::TargetKind::Live,
            version: crate::rules::ClientVersion::new(12, 1, 5),
            interface: 120_105,
            exe_name: None,
            label: None,
            rules: crate::rules::ClientRules::default(),
            judge: String::new(),
        }
    }

    #[test]
    fn a_scope_is_sorted_unique_and_shows_its_selector() {
        let retail = target("retail", Some(RETAIL));
        let ptr = target("ptr", Some(PTR));
        let new = target("xptr", None);
        let scope = TargetScope::of(Some("mainline"), &[&retail, &new, &ptr, &retail]);
        assert_eq!(
            scope,
            TargetScope {
                all: false,
                selector: Some("mainline".to_owned()),
                targets: vec![
                    client("ptr", PTR),
                    client("retail", RETAIL),
                    IntentTarget {
                        target: TargetRef::Provisional("xptr".to_owned()),
                        target_id: "xptr".to_owned(),
                    },
                ],
            }
        );
        assert_eq!(scope.validate(), Ok(()));
        assert_eq!(scope.flag(), Some("mainline"));
        for every in [None, Some("all"), Some("ALL")] {
            let scope = TargetScope::of(every, &[&retail]);
            assert!(
                scope.all && scope.selector.is_none(),
                "{every:?}: {scope:?}"
            );
            assert_eq!(scope.flag(), None);
        }
        let named_all = target("all", Some(ERA));
        let scope = TargetScope::of(Some("all"), &[&named_all]);
        assert_eq!(
            (scope.all, scope.flag()),
            (false, Some("all")),
            "a client named all wins over the keyword"
        );
        assert_eq!(
            TargetScope::of(Some("vanilla"), &[&retail]).flag(),
            Some("retail")
        );
    }
}
