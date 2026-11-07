//! What `omaddons apply` executes a stored plan with (M4-D23), before the command around
//! it exists: a plan's typed intent run by its command's own `run`, the typed outcome
//! that returns ([`StorableOutcome`]), the wrapper `apply`'s data is ([`ApplyData`]), and
//! the per-target report ([`TargetOutcome`], M4-D61). Nothing here parses an argv: an
//! intent becomes its command's arguments by conversion (`From<&XIntent> for XArgs`, in
//! `intent.rs`), and an intent can name no command but a storable one.

use std::borrow::Cow;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use jiff::Timestamp;
use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::Serialize;
use serde_json::json;

use super::adopt::{self, AdoptArgs, AdoptData, AdoptOutcome};
use super::approval::Approval;
use super::backup::{
    self, BackupPruneArgs, BackupPruneData, BackupPruneOutcome, BackupRestoreArgs,
    BackupRestoreData, BackupRestoreOutcome,
};
use super::copy::{self, CopyArgs, CopyOutcome};
use super::history::MIN_PREFIX;
use super::install::{self, InstallArgs, InstallData, InstallOutcome};
use super::intent::{AbsPath, StoredIntent};
use super::outcome::{ItemStatus, TargetsData};
use super::pin::{self, PinArgs, PinOutcome, UnpinArgs};
use super::precondition;
use super::remove::{self, RemoveArgs, RemoveData, RemoveOutcome, RemoveStatus};
use super::sync::{self, SyncArgs, SyncOutcome};
use super::undo::{self, UndoArgs, UndoData, UndoOutcome, UndoTxStatus};
use super::update::{self, UpdateArgs, UpdateOutcome};
use super::world::World;
use crate::cli::Context;
use crate::contract::actions::Action;
use crate::error::{CommandHint, Error, ErrorCode, PlanStaleSource, Result, Suggestion, Warning};
use crate::fsx::{HolderInfo, Lease, LeaseError, LeaseGuard, ScopeTable};
use crate::limits::{PLAN_EVIDENCE_LINES_MAX, PLAN_LEASE_TIMEOUT};
use crate::model::{PlanId, TargetKey, TxId};
use crate::output::{Outcome, Style};
use crate::state::history::{self, PlanEvidence, PlanMatch};
use crate::state::paths::Paths;
use crate::state::plans::{self, Found, StoredPlan};

/// One row per storable command, in the plan's order (M4-D22): its intent variant and
/// path, its arguments, its own `run`, the outcome that returns and that outcome's data.
/// Generates [`STORABLE_PATHS`], [`StorableOutcome`], [`StoredIntent::execute`], the
/// `applied.command` tags and [`ApplyData`].
macro_rules! storable_commands {
    ($($variant:ident, $path:literal, $args:ty, $run:path, $outcome:ty, $data:ty;)+) => {
        /// The storable command paths, in the plan's order.
        pub const STORABLE_PATHS: &[&str] = &[$($path),+];

        /// What a storable command's own `run` returned, before anything renders.
        #[derive(Debug)]
        pub enum StorableOutcome {
            $($variant($outcome),)+
        }

        impl StoredIntent {
            /// Runs the intent's command with the arguments it converts to, in `ctx`
            /// (whose target, policy and binding `apply` derives from the plan).
            pub fn execute(&self, ctx: &Context) -> Result<StorableOutcome> {
                match self {
                    $(Self::$variant(intent) => {
                        $run(ctx, &<$args>::from(intent)).map(StorableOutcome::$variant)
                    })+
                }
            }
        }

        impl StorableOutcome {
            /// The executed command's canonical path.
            pub fn command(&self) -> &'static str {
                match self {
                    $(Self::$variant(o) => o.command(),)+
                }
            }

            pub fn warnings(&self) -> &[Warning] {
                match self {
                    $(Self::$variant(o) => o.warnings(),)+
                }
            }

            pub fn commands(&self) -> &[CommandHint] {
                match self {
                    $(Self::$variant(o) => o.commands(),)+
                }
            }

            pub fn partial(&self) -> bool {
                match self {
                    $(Self::$variant(o) => o.partial(),)+
                }
            }

            pub fn assertion_failure(&self) -> Option<Error> {
                match self {
                    $(Self::$variant(o) => o.assertion_failure(),)+
                }
            }

            pub fn render_human(&self, w: &mut dyn Write, style: &Style) -> std::io::Result<()> {
                match self {
                    $(Self::$variant(o) => o.render_human(w, style),)+
                }
            }

            /// The binding token the executed run returned: a dry run's, else `None`.
            pub fn precondition(&self) -> Option<&str> {
                match self {
                    $(Self::$variant(o) => ResultData::precondition(o.data()),)+
                }
            }

            /// One entry per target of the result, sorted by target id (M4-D61).
            pub fn target_outcomes(&self) -> Vec<TargetOutcome> {
                match self {
                    $(Self::$variant(o) => outcomes_of(o.data()),)+
                }
            }

            /// The executed outcome, as its dispatch arm logs it.
            fn inner_events(&self) -> &dyn super::SideEvents {
                match self {
                    $(Self::$variant(o) => o as &dyn super::SideEvents,)+
                }
            }
        }

        /// What the executed run's dispatch arm would log (`-v`), so `apply` logs exactly
        /// that.
        impl super::SideEvents for StorableOutcome {
            fn notes(&self) -> &[String] {
                self.inner_events().notes()
            }

            fn locks(&self) -> Vec<(TxId, u64)> {
                self.inner_events().locks()
            }
        }

        /// `applied.command`'s values: one single-value string per storable command.
        pub mod tag {
            use std::borrow::Cow;

            use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
            use serde::Serialize;

            $(
                #[doc = concat!("`", $path, "`.")]
                #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
                pub enum $variant {
                    #[serde(rename = $path)]
                    $variant,
                }

                /// Inline, so it adds no definition; its name only names its wrapper's.
                impl JsonSchema for $variant {
                    fn inline_schema() -> bool {
                        true
                    }

                    fn schema_name() -> Cow<'static, str> {
                        stringify!($variant).into()
                    }

                    fn json_schema(_: &mut SchemaGenerator) -> Schema {
                        json_schema!({ "type": "string", "const": $path })
                    }
                }
            )+
        }

        /// `apply`'s data: one complete wrapper per storable command, `applied.command`
        /// naming it and `result` holding its data. Its schema is a `oneOf` of them, so a
        /// command and another command's result never validate together.
        #[derive(Debug, Clone, Serialize)]
        #[serde(untagged)]
        pub enum ApplyData {
            $($variant(Wrapper<tag::$variant, $data>),)+
        }

        impl JsonSchema for ApplyData {
            fn schema_name() -> Cow<'static, str> {
                "ApplyData".into()
            }

            fn json_schema(g: &mut SchemaGenerator) -> Schema {
                let variants = vec![$(g.subschema_for::<Wrapper<tag::$variant, $data>>(),)+];
                json_schema!({ "oneOf": variants })
            }
        }

        impl ApplyData {
            /// The wrapper of `executed`'s data, which it clones: the outcome stays whole
            /// for rendering.
            pub fn new(
                plan_id: PlanId,
                display_argv: Vec<String>,
                approved: Option<Approval>,
                executed: &StorableOutcome,
                outcomes: Vec<TargetOutcome>,
            ) -> Self {
                match executed {
                    $(StorableOutcome::$variant(o) => Self::$variant(Wrapper {
                        plan_id,
                        applied: Applied {
                            command: tag::$variant::$variant,
                            display_argv,
                        },
                        approved,
                        result: o.data().clone(),
                        outcomes,
                    }),)+
                }
            }
        }
    };
}

storable_commands! {
    Install,       "install",        InstallArgs,       install::run,           InstallOutcome,       InstallData;
    Update,        "update",         UpdateArgs,        update::run,            UpdateOutcome,        TargetsData;
    Pin,           "pin",            PinArgs,           pin::pin,               PinOutcome,           TargetsData;
    Unpin,         "unpin",          UnpinArgs,         pin::unpin,             PinOutcome,           TargetsData;
    Adopt,         "adopt",          AdoptArgs,         adopt::run,             AdoptOutcome,         AdoptData;
    Sync,          "sync",           SyncArgs,          sync::run,              SyncOutcome,          TargetsData;
    Copy,          "copy",           CopyArgs,          copy::run,              CopyOutcome,          TargetsData;
    Remove,        "remove",         RemoveArgs,        remove::run,            RemoveOutcome,        RemoveData;
    Undo,          "undo",           UndoArgs,          undo::run,              UndoOutcome,          UndoData;
    BackupRestore, "backup restore", BackupRestoreArgs, backup::restore_backup, BackupRestoreOutcome, BackupRestoreData;
    BackupPrune,   "backup prune",   BackupPruneArgs,   backup::prune_backups,  BackupPruneOutcome,   BackupPruneData;
}

/// One storable command's `apply` data.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[schemars(rename = "Apply{C}")]
pub struct Wrapper<C, D> {
    pub plan_id: PlanId,
    pub applied: Applied<C>,
    /// `--dry-run`: the approval stored with the plan (M4-D24); `null` on a real run.
    pub approved: Option<Approval>,
    /// The executed command's own data.
    pub result: D,
    /// One per target of the result, sorted by target id (M4-D61).
    pub outcomes: Vec<TargetOutcome>,
}

/// What `apply` executed.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[schemars(rename = "Applied{C}")]
pub struct Applied<C> {
    /// The executed command's canonical path, as `COMMANDS` and the envelope name it.
    pub command: C,
    /// The argv a person would type for the plan's intent; documentation, never parsed.
    pub display_argv: Vec<String>,
}

/// What became of one target of an applied plan (M4-D61). Nothing calls a run over
/// several targets atomic: each target is reported on its own.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct TargetOutcome {
    /// The target's id.
    pub target: String,
    /// `null` only for an unregistered target under `--dry-run`.
    pub key: Option<TargetKey>,
    pub status: OutcomeStatus,
    /// The transaction committed on the target; `null` when none was.
    pub tx: Option<TxId>,
    /// The error that kept the target from committing (`stale` and `failed`); `null`
    /// otherwise.
    pub error_code: Option<ErrorCode>,
}

/// A target's [`TargetOutcome::status`]. A dry run commits nothing: its targets are
/// `unchanged` unless an error stopped them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OutcomeStatus {
    Committed,
    Unchanged,
    Stale,
    Failed,
}

/// What one target's part of a result says, for [`classify`].
#[derive(Debug, Clone, PartialEq, Eq)]
struct Facts {
    target: String,
    key: Option<TargetKey>,
    /// The transaction committed there.
    tx: Option<TxId>,
    /// The target's own error, else its first failed item's.
    error: Option<ErrorCode>,
    /// It changed something without a transaction (a prune deleted backups).
    done: bool,
}

/// One target's status: an error first (`plan_stale` is `stale`, any other `failed`),
/// then a transaction (`committed`), then a change without one (`committed`, `tx: null`),
/// else `unchanged`. Under `--dry-run` nothing is committed.
fn classify(dry_run: bool, facts: Facts) -> TargetOutcome {
    let (status, tx) = match (facts.tx, facts.error) {
        (Some(tx), _) if !dry_run => (OutcomeStatus::Committed, Some(tx)),
        (_, Some(ErrorCode::PlanStale)) => (OutcomeStatus::Stale, None),
        (_, Some(_)) => (OutcomeStatus::Failed, None),
        (_, None) if facts.done && !dry_run => (OutcomeStatus::Committed, None),
        (_, None) => (OutcomeStatus::Unchanged, None),
    };
    let error_code = match status {
        OutcomeStatus::Stale | OutcomeStatus::Failed => facts.error,
        OutcomeStatus::Committed | OutcomeStatus::Unchanged => None,
    };
    TargetOutcome {
        target: facts.target,
        key: facts.key,
        status,
        tx,
        error_code,
    }
}

/// `data`'s targets, classified and sorted by target id.
fn outcomes_of<D: ResultData>(data: &D) -> Vec<TargetOutcome> {
    let mut outcomes: Vec<TargetOutcome> = data
        .facts()
        .into_iter()
        .map(|facts| classify(data.dry_run(), facts))
        .collect();
    outcomes.sort_by(|a, b| (&a.target, a.key).cmp(&(&b.target, b.key)));
    outcomes
}

/// What [`StorableOutcome`] reads from each storable command's data.
trait ResultData {
    fn dry_run(&self) -> bool;
    /// The binding token a dry run returned.
    fn precondition(&self) -> Option<&str>;
    fn facts(&self) -> Vec<Facts>;
}

impl ResultData for TargetsData {
    fn dry_run(&self) -> bool {
        self.dry_run
    }

    fn precondition(&self) -> Option<&str> {
        self.precondition.as_deref()
    }

    fn facts(&self) -> Vec<Facts> {
        self.targets
            .iter()
            .map(|t| Facts {
                target: t.target.id.clone(),
                key: t.target.key,
                tx: t.tx,
                error: t.error.as_ref().map(|e| e.code).or_else(|| {
                    t.items
                        .iter()
                        .find(|i| i.status == ItemStatus::Failed)
                        .and_then(|i| i.error.as_ref().map(|e| e.code))
                }),
                done: false,
            })
            .collect()
    }
}

impl ResultData for AdoptData {
    fn dry_run(&self) -> bool {
        match self {
            Self::Report(_) => false,
            Self::Adopted(data) => data.dry_run,
        }
    }

    fn precondition(&self) -> Option<&str> {
        match self {
            Self::Report(_) => None,
            Self::Adopted(data) => data.precondition.as_deref(),
        }
    }

    /// A report adopts nothing and stores no plan: it has no targets to report.
    fn facts(&self) -> Vec<Facts> {
        match self {
            Self::Report(_) => Vec::new(),
            Self::Adopted(data) => data.facts(),
        }
    }
}

impl ResultData for InstallData {
    fn dry_run(&self) -> bool {
        self.dry_run
    }

    fn precondition(&self) -> Option<&str> {
        self.precondition.as_deref()
    }

    fn facts(&self) -> Vec<Facts> {
        vec![Facts {
            target: self.target.id.clone(),
            key: self.target.key,
            tx: self.tx,
            error: self
                .items
                .iter()
                .find(|i| i.status == ItemStatus::Failed)
                .and_then(|i| i.error.as_ref().map(|e| e.code)),
            done: false,
        }]
    }
}

impl ResultData for RemoveData {
    fn dry_run(&self) -> bool {
        self.dry_run
    }

    fn precondition(&self) -> Option<&str> {
        self.precondition.as_deref()
    }

    fn facts(&self) -> Vec<Facts> {
        vec![Facts {
            target: self.target.id.clone(),
            key: Some(self.target.key),
            tx: self.tx,
            error: self
                .items
                .iter()
                .find(|i| i.status == RemoveStatus::Failed)
                .and_then(|i| i.error.as_ref().map(|e| e.code)),
            done: false,
        }]
    }
}

impl ResultData for UndoData {
    fn dry_run(&self) -> bool {
        self.dry_run
    }

    fn precondition(&self) -> Option<&str> {
        self.precondition.as_deref()
    }

    /// One entry per client a reverted transaction was on: the last undo committed
    /// there, else its first error. A repair's undo names no single client: one entry
    /// with no key and no id stands for those.
    fn facts(&self) -> Vec<Facts> {
        let mut facts: Vec<Facts> = Vec::new();
        for t in &self.transactions {
            let key = t.target.as_ref().map(|target| target.key);
            let at = match facts.iter().position(|f| f.key == key) {
                Some(at) => at,
                None => {
                    facts.push(Facts {
                        target: t
                            .target
                            .as_ref()
                            .map(|target| target.id.clone())
                            .unwrap_or_default(),
                        key,
                        tx: None,
                        error: None,
                        done: false,
                    });
                    facts.len() - 1
                }
            };
            let entry = &mut facts[at];
            match (t.status, t.tx) {
                (UndoTxStatus::Undone, Some(tx)) => entry.tx = Some(tx),
                (UndoTxStatus::Undone, None) => {}
                (UndoTxStatus::Failed, _) => {
                    if entry.error.is_none() {
                        entry.error = t.error.as_ref().map(|e| e.code);
                    }
                }
            }
        }
        // A client with an undo committed is committed, whatever else failed there.
        for entry in &mut facts {
            if entry.tx.is_some() {
                entry.error = None;
            }
        }
        facts
    }
}

impl ResultData for BackupRestoreData {
    fn dry_run(&self) -> bool {
        self.dry_run
    }

    fn precondition(&self) -> Option<&str> {
        self.precondition.as_deref()
    }

    /// A restore that fails fails the command: its one target either committed or, on a
    /// dry run, changed nothing.
    fn facts(&self) -> Vec<Facts> {
        vec![Facts {
            target: self.target.id.clone(),
            key: self.target.key,
            tx: self.tx,
            error: None,
            done: false,
        }]
    }
}

impl ResultData for BackupPruneData {
    fn dry_run(&self) -> bool {
        self.dry_run
    }

    fn precondition(&self) -> Option<&str> {
        self.precondition.as_deref()
    }

    /// A prune writes no transaction: a client whose backups it deleted is committed with
    /// `tx: null`.
    fn facts(&self) -> Vec<Facts> {
        self.targets
            .iter()
            .map(|t| Facts {
                target: t.target.id.clone(),
                key: t.target.key,
                tx: None,
                error: t.error.as_ref().map(|e| e.code),
                done: !t.pruned.is_empty(),
            })
            .collect()
    }
}

// ---------------------------------------------------------------------------------------
// The command (M4-D23, M4-D61).

/// `apply`'s own arguments.
#[derive(Debug, Clone, Default, clap::Args)]
pub struct ApplyArgs {
    /// The plan a --dry-run stored (data.plan_id): its id, or a unique prefix of 8+
    /// characters
    #[arg(value_name = "PLAN_ID")]
    pub plan_id: String,
}

/// `apply`'s outcome: the executed command's, wrapped.
#[derive(Debug)]
pub struct ApplyOutcome {
    data: ApplyData,
    executed: StorableOutcome,
    /// The executed outcome's, then (a dry run) the `apply` hint.
    commands: Vec<CommandHint>,
    /// `apply`'s own `-v` notes, then the executed outcome's.
    notes: Vec<String>,
    /// A dry run's last human line.
    dry_run_hint: Option<String>,
}

impl ApplyOutcome {
    fn new(
        plan: &StoredPlan,
        dry_run: bool,
        executed: StorableOutcome,
        outcomes: Vec<TargetOutcome>,
        mut notes: Vec<String>,
    ) -> Self {
        let data = ApplyData::new(
            plan.plan_id,
            plan.display_argv.clone(),
            dry_run.then(|| plan.approval.clone()),
            &executed,
            outcomes,
        );
        let mut commands = executed.commands().to_vec();
        let dry_run_hint = dry_run.then(|| {
            commands.push(apply_hint(plan.plan_id, plan.intent.destructive()));
            format!(
                "Plan {} still applies: omaddons apply {}",
                super::history::short_id(&plan.plan_id.to_string()),
                plan.plan_id
            )
        });
        notes.extend(super::SideEvents::notes(&executed).iter().cloned());
        Self {
            data,
            executed,
            commands,
            notes,
            dry_run_hint,
        }
    }
}

/// `omaddons apply <plan_id>`, as destructive as the plan's command.
pub fn apply_hint(plan_id: PlanId, destructive: bool) -> CommandHint {
    CommandHint::new(
        Action::Apply,
        [
            "omaddons".to_owned(),
            "apply".to_owned(),
            plan_id.to_string(),
        ],
    )
    .with_destructive(destructive)
}

impl Outcome for ApplyOutcome {
    type Data = ApplyData;

    fn command(&self) -> &'static str {
        "apply"
    }

    fn data(&self) -> &ApplyData {
        &self.data
    }

    fn warnings(&self) -> &[Warning] {
        self.executed.warnings()
    }

    fn commands(&self) -> &[CommandHint] {
        &self.commands
    }

    fn partial(&self) -> bool {
        self.executed.partial()
    }

    fn assertion_failure(&self) -> Option<Error> {
        self.executed.assertion_failure()
    }

    /// The executed command's own output, then a dry run's hint line.
    fn render_human(&self, w: &mut dyn Write, style: &Style) -> std::io::Result<()> {
        self.executed.render_human(w, style)?;
        if let Some(line) = &self.dry_run_hint {
            writeln!(w, "{line}")?;
        }
        Ok(())
    }
}

impl super::SideEvents for ApplyOutcome {
    fn notes(&self) -> &[String] {
        &self.notes
    }

    fn locks(&self) -> Vec<(TxId, u64)> {
        super::SideEvents::locks(&self.executed)
    }
}

/// Runs `apply`: the plan `args` names, checked, executed by its command's own `run` in
/// a context derived from it, and wrapped. A real run holds the plan's lease from before
/// its checks until every target's outcome is known, and releases it before rendering.
pub fn run(ctx: &Context, args: &ApplyArgs) -> Result<ApplyOutcome> {
    refuse_own_flags(ctx)?;
    let paths = Paths::from_env()?;
    let path = resolve(ctx, &paths, &args.plan_id)?;
    let mut plan = plans::load(&path)?;
    let lease = if ctx.dry_run {
        None
    } else {
        let guard = take_lease(&paths, &plan.plan_id, PLAN_LEASE_TIMEOUT)?;
        // What the wait may have let another applier commit is read again.
        plan = reload(&path, &plan.plan_id)?;
        Some(guard)
    };
    let mut notes = Vec::new();
    check_lifecycle_now(&paths, &plan, &mut notes)?;
    refuse_offline(ctx, &plan)?;
    let applied = applied_context(ctx, &plan);
    let executed = plan.intent.execute(&applied)?;
    if ctx.dry_run {
        check_dry_run_token(&plan, &executed)?;
    }
    let outcomes = executed.target_outcomes();
    drop(lease);
    Ok(ApplyOutcome::new(
        &plan,
        ctx.dry_run,
        executed,
        outcomes,
        notes,
    ))
}

/// `-t` and `--while-running` refused before anything is read: the plan names its
/// clients and the running policy it was approved under. (`--if-match` never gets here:
/// `Context::new` refuses it, the plan carrying its own token.)
fn refuse_own_flags(ctx: &Context) -> Result<()> {
    if ctx.target.is_some() {
        let fix = crate::cli::without_target(&ctx.argv).map_or_else(
            || Suggestion::text("drop the -t option"),
            Suggestion::command,
        );
        return Err(Error::new(
            ErrorCode::Usage,
            "apply runs the plan's own targets: drop -t",
            fix,
        )
        .user_verb()
        .details(json!({ "flag": "--target" })));
    }
    if ctx.while_running {
        let argv = crate::cli::without_switch(&ctx.argv, "--while-running");
        return Err(Error::new(
            ErrorCode::Usage,
            "a plan's running-client policy is part of what was approved: drop --while-running, or preview again with it",
            Suggestion::command(crate::cli::fix_argv(&argv)),
        )
        .user_verb()
        .details(json!({ "flag": "--while-running" })));
    }
    Ok(())
}

/// The plan file `given` names: a full id or a unique prefix of 8+ characters.
fn resolve(ctx: &Context, paths: &Paths, given: &str) -> Result<PathBuf> {
    let found = if given.chars().count() < MIN_PREFIX {
        None
    } else {
        Some(plans::find(paths, given)?)
    };
    let cut = match found {
        Some(Found::One { path, .. }) => return Ok(path),
        Some(Found::Many(ids)) => return Err(ambiguous(given, &ids)),
        Some(Found::None { cut }) => cut,
        None => false,
    };
    if let Some(hint) = folder_hint(ctx, given) {
        return Err(hint);
    }
    if found.is_none() {
        return Err(Error::new(
            ErrorCode::Usage,
            format!("a plan id prefix needs at least {MIN_PREFIX} characters, not {given:?}"),
            Suggestion::command(["omaddons", "help", "apply"]),
        ));
    }
    let more = if cut {
        "; the plan directory holds more files than one listing reads, so give the full id"
    } else {
        ""
    };
    Err(Error::new(
        ErrorCode::PlanNotFound,
        format!(
            "no stored plan has an id starting with {given}; plans last 24 hours and an emptied cache removes them: run the command again with --dry-run{more}"
        ),
        Suggestion::command(["omaddons", "help", "apply"]),
    )
    .alternative(Suggestion::command(["omaddons", "adopt", "--apply"]))
    .details(json!({ "prefix": given })))
}

/// What `apply <word>` meant when `word` names no plan (M4-D27): an addon folder on a
/// client that omaddons did not install, to adopt where it is. `not_found`, with the help
/// first (adopting changes something) and one `adopt` per client holding the folder.
/// `None` when no client holds one, or when the clients cannot be read: the plan id is
/// unknown either way, and the caller's own refusal stands. Discovery writes nothing
/// under `<state>`.
fn folder_hint(ctx: &Context, given: &str) -> Option<Error> {
    let world = World::load(ctx).ok()?;
    let targets = world.select_many(ctx).ok()?;
    let records = world.records().ok()?;
    let fixes = adopt::hints::adopt_fixes(&world, &records, &targets, given);
    if fixes.is_empty() {
        return None;
    }
    let error = Error::new(
        ErrorCode::NotFound,
        format!(
            "{given} is no stored plan; it is an addon folder omaddons did not install: adopt it, or give a plan id from a --dry-run"
        ),
        Suggestion::command(["omaddons", "help", "apply"]),
    );
    Some(fixes.into_iter().fold(error, Error::alternative))
}

/// Several plans start with `given`: the first's dry run is the fix, each an alternative.
fn ambiguous(given: &str, ids: &[PlanId]) -> Error {
    let names: Vec<String> = ids.iter().map(ToString::to_string).collect();
    let apply = |id: &str| Suggestion::command(["omaddons", "apply", id]);
    let first = names.first().cloned().unwrap_or_default();
    let mut error = Error::new(
        ErrorCode::RefAmbiguous,
        format!(
            "{} stored plans have ids starting with {given}: {}",
            names.len(),
            names.join(", ")
        ),
        Suggestion::command(["omaddons", "apply", first.as_str(), "--dry-run"]),
    )
    .details(json!({ "prefix": given, "candidates": names }));
    for id in &names {
        error = error.alternative(apply(id));
    }
    error
}

/// The plan's lease (M4-D61): one applier at a time, waiting at most `timeout`.
fn take_lease(paths: &Paths, id: &PlanId, timeout: Duration) -> Result<LeaseGuard> {
    let path = paths.plan_lease(id);
    let scopes = ScopeTable::new(paths, &[], None)?;
    Lease::acquire(&scopes, &path, "apply", timeout).map_err(|err| match err {
        LeaseError::Busy { holder, waited } => {
            busy(id, holder.as_ref(), waited, crate::clock::now())
        }
        LeaseError::Refused(why) => Error::new(
            ErrorCode::ScopeDenied,
            format!("plan {id}'s lease cannot be taken: {why}"),
            Suggestion::command(["omaddons", "status"]),
        ),
        LeaseError::Io(err) => Error::io(&path, &err, Suggestion::command(["omaddons", "status"])),
    })
}

/// `locked`: another `apply` held the plan's lease for the whole wait.
fn busy(id: &PlanId, holder: Option<&HolderInfo>, waited: Duration, now: Timestamp) -> Error {
    let by = holder.map_or_else(
        || "another omaddons process".to_owned(),
        |h| {
            let age = age_s(h.started_at, now);
            format!("omaddons {} (pid {}, for {age} s)", h.command, h.pid)
        },
    );
    let holder = holder.map(|h| {
        json!({
            "pid": h.pid,
            "command": h.command,
            "started_at": h.started_at.to_string(),
            "age_s": age_s(h.started_at, now),
        })
    });
    Error::new(
        ErrorCode::Locked,
        format!(
            "plan {id} is being applied by {by}; waited {} s",
            waited.as_secs()
        ),
        Suggestion::text("wait for the other apply to finish, then retry"),
    )
    .details(json!({ "lease": id.to_string(), "holder": holder }))
}

/// Whole seconds from `started_at` to `now`, never negative.
fn age_s(started_at: Timestamp, now: Timestamp) -> i64 {
    now.duration_since(started_at).as_secs().max(0)
}

/// The plan read again under its lease; a plan removed while it waited is
/// `plan_not_found`.
fn reload(path: &Path, id: &PlanId) -> Result<StoredPlan> {
    match std::fs::symlink_metadata(path) {
        // ubs:ignore — an io::ErrorKind, not a secret
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Err(Error::new(
            ErrorCode::PlanNotFound,
            format!("plan {id} was removed while this apply waited for it"),
            Suggestion::text("preview the change again: run its command with --dry-run"),
        )
        .details(json!({ "plan_id": id.to_string(), "path": path.display().to_string() }))),
        _ => plans::load(path),
    }
}

/// [`check_lifecycle`] with what it reads: the history scan (the one `plans show` reads,
/// so the two never disagree), the clock, this build and the plan's local inputs. A scan
/// that found nothing but was cut, or met lines it could not read or attribute, is a `-v`
/// note, not a refusal: the token's generations still refuse a second application.
fn check_lifecycle_now(paths: &Paths, plan: &StoredPlan, notes: &mut Vec<String>) -> Result<()> {
    let id = plan.plan_id.to_string();
    let horizon = plans::evidence_horizon(Some(plan.created_at), &id, crate::clock::now());
    let evidence = history::plan_evidence(
        paths,
        &PlanMatch::Exact(plan.plan_id),
        horizon,
        PLAN_EVIDENCE_LINES_MAX,
    )?;
    if evidence.lines.is_empty() {
        let still = "the token's generations still refuse a second application";
        if !evidence.complete() {
            notes.push(format!(
                "the history scan for plan {id} stopped after {} lines; {still}",
                evidence.lines_read
            ));
        }
        if evidence.unreadable + evidence.unattributed > 0 {
            notes.push(format!(
                "the history scan for plan {id} met {} lines it could not read and {} it could not attribute; {still}",
                evidence.unreadable, evidence.unattributed
            ));
        }
    }
    let missing = plan
        .intent
        .inputs()
        .into_iter()
        .find(|input| plans::input_is_gone(input.as_path()));
    check_lifecycle(
        plan,
        crate::clock::now(),
        crate::build_info::BUILD_ID,
        &evidence,
        missing,
    )
}

/// The refusals of a plan that can no longer apply, in order: `already_applied`,
/// `expired`, `binary_changed`, `input_missing`. Each is `plan_stale`, never retryable,
/// whose fix is the plan's dry run.
fn check_lifecycle(
    plan: &StoredPlan,
    now: Timestamp,
    build_id: &str,
    evidence: &PlanEvidence,
    missing: Option<&AbsPath>,
) -> Result<()> {
    let id = plan.plan_id;
    let stale = |message: String, details: serde_json::Value| {
        Error::plan_stale(
            PlanStaleSource::Apply,
            message,
            Suggestion::command(precondition::preview_argv(&plan.display_argv)),
        )
        .details(details)
    };
    if !evidence.lines.is_empty() {
        let txs: Vec<String> = evidence.lines.iter().map(|l| l.tx.to_string()).collect();
        return Err(stale(
            format!(
                "plan {id} was already applied: history records tx {}; nothing was changed",
                txs.join(", ")
            ),
            json!({ "changed": [], "reason": "already_applied", "txs": txs }),
        ));
    }
    if plan.is_expired(now) {
        return Err(stale(
            format!(
                "plan {id} expired at {} (plans last 24 hours); nothing was changed",
                plan.expires_at
            ),
            json!({ "changed": [], "reason": "expired" }),
        ));
    }
    if plan.build_id != build_id {
        return Err(stale(
            format!(
                "plan {id} was made by another omaddons build; nothing was changed: preview it again with this one"
            ),
            json!({ "changed": [], "reason": "binary_changed" }),
        ));
    }
    if let Some(path) = missing {
        return Err(stale(
            format!("plan {id} installs from {path}, which is gone; nothing was changed"),
            json!({ "changed": [], "reason": "input_missing", "path": path.as_str() }),
        ));
    }
    Ok(())
}

/// Whether applying `plan` needs the network: a release it installs from a URL, or
/// from a deferred asset resolved for each download. Local files and records need none.
fn needs_network(plan: &StoredPlan) -> bool {
    plan.releases
        .iter()
        .any(|r| r.url.is_some() || r.source.is_some())
}

/// `offline` before anything runs, when an offline `apply` could not check the plan's
/// sources again (M4-D20): offline-ness is `apply`'s own, never the preview's.
fn refuse_offline(ctx: &Context, plan: &StoredPlan) -> Result<()> {
    if ctx.offline && needs_network(plan) {
        return Err(Error::new(
            ErrorCode::Offline,
            format!(
                "plan {} installs releases from their sources, which apply checks again first; --offline forbids that",
                plan.plan_id
            ),
            Suggestion::text("run without --offline and with OMADDONS_OFFLINE unset"),
        ));
    }
    Ok(())
}

/// The context the plan's command runs in: its own path and approved argv, the clients
/// and running policy the plan names, bound to its token, revalidating every source,
/// recording the plan (M4-D20, M4-D25). The actor, the output, offline-ness and the dry
/// run are `apply`'s; the reason is `apply`'s, else the preview's.
fn applied_context(ctx: &Context, plan: &StoredPlan) -> Context {
    Context {
        command: plan.intent.command_path(),
        argv: plan.display_argv.clone(),
        target: plan.intent.target_selector(),
        while_running: plan.intent.while_running(),
        if_match: Some(plan.precondition.token.clone()),
        force_revalidate: true,
        applying: Some(plan.plan_id),
        reason: ctx.reason.clone().or_else(|| plan.reason.clone()),
        ..ctx.clone()
    }
}

/// `apply --dry-run`'s staleness: the fresh preview's token against the stored one. The
/// engine commands' bound dry runs refuse on their own; `remove`, `undo` and the backup
/// commands preview afresh, so this is how a changed world reaches them.
fn check_dry_run_token(plan: &StoredPlan, executed: &StorableOutcome) -> Result<()> {
    let changed = match executed.precondition() {
        Some(fresh) => precondition::changed_parts(&plan.precondition.token, fresh),
        None => precondition::Part::ALL.to_vec(),
    };
    if changed.is_empty() {
        return Ok(());
    }
    let names: Vec<&str> = changed.iter().map(|p| p.wire()).collect();
    Err(Error::plan_stale(
        PlanStaleSource::Apply,
        format!(
            "plan {} no longer describes the world ({} changed); nothing was changed: preview it again",
            plan.plan_id,
            names.join(", ")
        ),
        Suggestion::command(precondition::preview_argv(&plan.display_argv)),
    )
    .details(json!({ "changed": names })))
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::*;
    use crate::app::adopt::AdoptReport;
    use crate::app::backup::{BackupPruneTarget, RestoreMode, RestoreSource};
    use crate::app::history::TargetRef as HistoryTarget;
    use crate::app::install::InstallItem;
    use crate::app::outcome::{ItemError, ItemResult, Summary, TargetRef, TargetResult};
    use crate::app::remove::RemoveItem;
    use crate::app::undo::{Reverted, SavedVariables, UndoTx};
    use crate::backup::prune::Pruned;
    use crate::backup::{BackupKind, Integrity};
    use crate::engine::SvPolicy;
    use crate::engine::restore::Exchange;
    use crate::error::{ErrorBody, Suggestion};
    use crate::model::{Actor, ReloadRequired, TxKind};

    const RETAIL: &str = "01JB8Q2VA0000000000000000A";
    const CLASSIC: &str = "01JB8Q2VA0000000000000000B";

    fn key(k: &str) -> TargetKey {
        k.parse().unwrap()
    }

    fn tx(n: u8) -> TxId {
        format!("01JB8Q2VC000000000000000{n:02}").parse().unwrap()
    }

    fn error(code: ErrorCode) -> Error {
        Error::new(code, "what happened", Suggestion::text("what to do"))
    }

    fn item_error(code: ErrorCode) -> ItemError {
        ItemError::from(&error(code))
    }

    fn target_ref(id: &str, k: &str) -> TargetRef {
        TargetRef {
            id: id.to_owned(),
            key: Some(key(k)),
            mode: None,
        }
    }

    fn item(status: ItemStatus, code: Option<ErrorCode>) -> ItemResult {
        ItemResult {
            request: "foo".into(),
            name: Some("Foo".into()),
            r#ref: None,
            status,
            skip_reason: None,
            duplicate_of: None,
            reason: None,
            from_version: None,
            to_version: None,
            folders: None,
            required_by: None,
            deps: crate::app::outcome::ItemDeps::default(),
            error: code.map(item_error),
        }
    }

    /// One target of a multi-target result.
    fn on(
        id: &str,
        k: &str,
        tx: Option<TxId>,
        failed: Option<ErrorCode>,
        items: Vec<ItemResult>,
    ) -> TargetResult {
        TargetResult {
            target: target_ref(id, k),
            tx,
            kind: tx.map(|_| TxKind::Update),
            no_section: false,
            items,
            reload_required: ReloadRequired::None,
            atomic_swap: None,
            creates_addons_dir: false,
            error: failed.map(|code| ErrorBody::from(error(code))),
        }
    }

    fn targets(dry_run: bool, targets: Vec<TargetResult>) -> TargetsData {
        TargetsData {
            dry_run,
            run: None,
            targets,
            summary: Summary::default(),
            precondition: dry_run.then(|| "pc1:targets".to_owned()),
            plan_id: None,
            plan_expires_at: None,
            approval: None,
        }
    }

    fn install_item(status: ItemStatus, code: Option<ErrorCode>) -> InstallItem {
        InstallItem {
            request: "foo".into(),
            r#ref: None,
            name: Some("Foo".into()),
            status,
            skip_reason: None,
            duplicate_of: None,
            fields: Vec::new(),
            from_version: None,
            to_version: None,
            version_confidence: None,
            channel: None,
            folders: Default::default(),
            error: code.map(item_error),
            reason: None,
            required_by: Vec::new(),
            bridge_dependencies: Vec::new(),
            unresolved: Vec::new(),
            asset_url: None,
            optional_dependencies: None,
        }
    }

    fn install(dry_run: bool, tx: Option<TxId>, items: Vec<InstallItem>) -> InstallData {
        InstallData {
            dry_run,
            target: target_ref("retail", RETAIL),
            tx,
            kind: tx.map(|_| TxKind::Install),
            items,
            reload_required: ReloadRequired::None,
            atomic_swap: None,
            creates_addons_dir: false,
            precondition: dry_run.then(|| "pc1:install".to_owned()),
            plan_id: None,
            plan_expires_at: None,
            approval: None,
            client_dir: None,
        }
    }

    fn remove(tx: Option<TxId>, failed: Option<ErrorCode>) -> RemoveData {
        RemoveData {
            dry_run: false,
            target: HistoryTarget {
                id: "retail".into(),
                key: key(RETAIL),
            },
            tx,
            items: vec![RemoveItem {
                request: "Foo".into(),
                name: Some("Foo".into()),
                r#ref: None,
                status: if failed.is_some() {
                    RemoveStatus::Failed
                } else {
                    RemoveStatus::Removed
                },
                version: None,
                folders: Vec::new(),
                error: failed.map(item_error),
            }],
            reload_required: ReloadRequired::None,
            atomic_swap: None,
            precondition: None,
            plan_id: None,
            plan_expires_at: None,
            approval: None,
        }
    }

    /// An undo of one transaction per `(id, key, undo tx, error)`.
    fn undo(dry_run: bool, txs: &[(&str, &str, Option<TxId>, Option<ErrorCode>)]) -> UndoData {
        let history_target = |id: &str, k: &str| HistoryTarget {
            id: id.to_owned(),
            key: key(k),
        };
        let transactions: Vec<UndoTx> = txs
            .iter()
            .enumerate()
            .map(|(n, (id, k, undone, failed))| UndoTx {
                target: Some(history_target(id, k)),
                reverted: tx(50 + u8::try_from(n).unwrap()),
                tx: *undone,
                status: if failed.is_some() {
                    UndoTxStatus::Failed
                } else {
                    UndoTxStatus::Undone
                },
                error: failed.map(item_error),
            })
            .collect();
        UndoData {
            dry_run,
            kind: crate::app::undo::UndoKind::Folders,
            reverted: Reverted {
                tx: tx(50),
                kind: TxKind::Install,
                at: "2026-10-29T12:00:00Z".parse().unwrap(),
                target: Some(history_target("retail", RETAIL)),
                targets: vec![history_target("retail", RETAIL)],
                actor: Actor::Agent,
                argv: vec!["omaddons".into(), "install".into()],
                reason: None,
            },
            tx: transactions.iter().find_map(|t| t.tx),
            folders: Default::default(),
            saved_variables: SavedVariables {
                policy: SvPolicy::Auto,
                restored: 0,
                unchanged: 0,
                deferred: 0,
                kept: 0,
            },
            changed_since: None,
            reload_required: ReloadRequired::None,
            atomic_swap: None,
            client_dir: None,
            run: None,
            reverted_run: None,
            repairs: None,
            doctor_run_id: None,
            transactions,
            precondition: None,
            plan_id: None,
            plan_expires_at: None,
            approval: None,
        }
    }

    fn restore(dry_run: bool, tx: Option<TxId>) -> BackupRestoreData {
        BackupRestoreData {
            dry_run,
            target: target_ref("retail", RETAIL),
            tx,
            kind: TxKind::Restore,
            mode: RestoreMode::Whole,
            backup: RestoreSource {
                id: "01M6Q2VA000000000000000000".parse().unwrap(),
                captured_at: "2026-10-29T12:00:00Z".parse().unwrap(),
                kind: BackupKind::Manual,
                integrity: Integrity::MetadataOnly,
            },
            files: None,
            before_restore: None,
            reload_required: ReloadRequired::Restart,
            precondition: dry_run.then(|| "pc1:restore".to_owned()),
            plan_id: None,
            plan_expires_at: None,
            approval: None,
            exchange: Exchange::Unverified,
            addons: None,
        }
    }

    /// A prune of `pruned` backups per `(id, key, pruned, error)`.
    fn prune(dry_run: bool, list: &[(&str, &str, usize, Option<ErrorCode>)]) -> BackupPruneData {
        BackupPruneData {
            dry_run,
            targets: list
                .iter()
                .map(|(id, k, pruned, failed)| BackupPruneTarget {
                    target: target_ref(id, k),
                    pruned: (0..*pruned)
                        .map(|_| Pruned {
                            id: crate::model::BackupId::new(),
                            kind: Some(BackupKind::Auto),
                            bytes: 10,
                        })
                        .collect(),
                    busy_backups: Vec::new(),
                    kept: 1,
                    reclaimed_for_space: false,
                    error: failed.map(item_error),
                })
                .collect(),
            precondition: None,
            plan_id: None,
            plan_expires_at: None,
            approval: None,
        }
    }

    fn out(
        target: &str,
        k: &str,
        status: OutcomeStatus,
        tx: Option<TxId>,
        error_code: Option<ErrorCode>,
    ) -> TargetOutcome {
        TargetOutcome {
            target: target.to_owned(),
            key: Some(key(k)),
            status,
            tx,
            error_code,
        }
    }

    // -----------------------------------------------------------------------------------
    // The command.

    const PLAN: &str = "01M9P0000000000000000000PA";
    const CREATED: &str = "2026-10-27T12:00:00Z";
    const BUILD: &str = "build-a";

    fn install_intent(while_running: bool) -> crate::app::intent::InstallIntent {
        crate::app::intent::InstallIntent {
            refs: vec![crate::app::intent::NormalizedRef::File(
                AbsPath::new("/home/u/Foo.zip".into()).unwrap(),
            )],
            target: crate::model::TargetRef::Key(key(RETAIL)),
            target_id: "retail".into(),
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
            while_running,
        }
    }

    fn stored_plan(intent: StoredIntent, releases: Vec<plans::StoredRelease>) -> StoredPlan {
        let created_at: Timestamp = CREATED.parse().unwrap();
        StoredPlan {
            plan_id: PLAN.parse().unwrap(),
            created_at,
            expires_at: created_at
                .checked_add(jiff::SignedDuration::from_hours(24))
                .unwrap(),
            tool_version: "0.1.0".into(),
            build_id: BUILD.into(),
            display_argv: intent.display_argv(),
            intent,
            actor: crate::model::Actor::Agent,
            reason: Some("from the preview".into()),
            precondition: plans::StoredPrecondition {
                token: format!("pc1:{}", "a".repeat(64)),
                parts: plans::StoredParts {
                    command: serde_json::json!("install"),
                    targets: serde_json::json!([]),
                    selection: serde_json::json!(null),
                    observed: serde_json::json!(null),
                },
            },
            releases,
            approval: Approval::nothing(&crate::app::approval::ApprovalFlags::default()),
            summary: Vec::new(),
        }
    }

    fn release(
        url: Option<&str>,
        source: Option<crate::sources::SourceId>,
    ) -> plans::StoredRelease {
        plans::StoredRelease {
            release_id: "gh:o/foo@v1".into(),
            source,
            key: source.map(|_| "elvui".to_owned()),
            url: url.map(str::to_owned),
            expected_hash: None,
            blob_sha256: None,
        }
    }

    fn ctx(argv: &[&str], reason: Option<&str>) -> Context {
        Context {
            output: crate::output::OutputConfig::machine(),
            command: "apply",
            actor: crate::model::Actor::Human,
            reason: reason.map(str::to_owned),
            argv: argv.iter().map(|a| (*a).to_owned()).collect(),
            offline: false,
            offline_by_env: false,
            dry_run: false,
            while_running: false,
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

    fn evidence(txs: &[TxId]) -> PlanEvidence {
        PlanEvidence {
            lines: txs
                .iter()
                .map(|tx| history::PlanLine {
                    plan: "01JB8Q2VR00000000000000001".parse().unwrap(),
                    tx: *tx,
                    target: Some(key(RETAIL)),
                    targets: Vec::new(),
                    kind: TxKind::Install,
                    run: None,
                    result: history::TxResult::Committed,
                    at: "2026-10-27T13:00:00Z".parse().unwrap(),
                })
                .collect(),
            unattributed: 0,
            unreadable: 0,
            lines_read: txs.len(),
            stop: history::ScanStop::Start,
        }
    }

    /// A lifecycle case: its name, now, the running build, the transactions history holds
    /// for the plan, whether its input is gone, and the refusal's details (`None`: none).
    type LifecycleCase = (
        &'static str,
        &'static str,
        &'static str,
        Vec<TxId>,
        bool,
        Option<Value>,
    );

    #[test]
    fn lifecycle_checks_in_order() {
        let plan = stored_plan(StoredIntent::Install(install_intent(false)), Vec::new());
        let gone = AbsPath::new("/home/u/Foo.zip".into()).unwrap();
        let (fresh, late) = ("2026-10-27T18:00:00Z", "2026-10-29T00:00:00Z");
        let applied = vec![tx(1)];
        let cases: Vec<LifecycleCase> = vec![
            ("all_clear", fresh, BUILD, vec![], false, None),
            (
                "already_applied",
                fresh,
                BUILD,
                applied.clone(),
                false,
                Some(
                    serde_json::json!({"changed": [], "reason": "already_applied", "txs": [tx(1).to_string()]}),
                ),
            ),
            (
                "expired",
                late,
                BUILD,
                vec![],
                false,
                Some(serde_json::json!({"changed": [], "reason": "expired"})),
            ),
            (
                "binary_changed",
                fresh,
                "build-b",
                vec![],
                false,
                Some(serde_json::json!({"changed": [], "reason": "binary_changed"})),
            ),
            (
                "input_missing",
                fresh,
                BUILD,
                vec![],
                true,
                Some(
                    serde_json::json!({"changed": [], "reason": "input_missing", "path": "/home/u/Foo.zip"}),
                ),
            ),
            (
                "applied_and_expired_is_already_applied",
                late,
                BUILD,
                applied.clone(),
                false,
                Some(
                    serde_json::json!({"changed": [], "reason": "already_applied", "txs": [tx(1).to_string()]}),
                ),
            ),
            (
                "expired_and_rebuilt_is_expired",
                late,
                "build-b",
                vec![],
                false,
                Some(serde_json::json!({"changed": [], "reason": "expired"})),
            ),
            (
                "rebuilt_and_input_gone_is_binary_changed",
                fresh,
                "build-b",
                vec![],
                true,
                Some(serde_json::json!({"changed": [], "reason": "binary_changed"})),
            ),
        ];
        for (name, now, build, txs, missing, want) in cases {
            let got = check_lifecycle(
                &plan,
                now.parse().unwrap(),
                build,
                &evidence(&txs),
                missing.then_some(&gone),
            );
            match (got, want) {
                (Ok(()), None) => {}
                (Err(error), Some(details)) => {
                    assert_eq!(
                        (error.code, error.retryable, &error.details),
                        (ErrorCode::PlanStale, false, &details),
                        "{name}: {error:?}"
                    );
                    assert_eq!(
                        error.fix.argv(),
                        Some(&precondition::preview_argv(&plan.display_argv)[..]),
                        "{name}: the plan's dry run"
                    );
                }
                (got, want) => unreachable!("{name}: got {got:?}, wanted {want:?}"),
            }
        }
    }

    #[test]
    fn applied_context_sets_each_field() {
        let plan = stored_plan(StoredIntent::Install(install_intent(true)), Vec::new());
        let mut own = ctx(&["omaddons", "apply", PLAN], None);
        own.dry_run = true;
        own.offline = true;
        let applied = applied_context(&own, &plan);
        assert_eq!(applied.command, "install");
        assert_eq!(applied.argv, plan.display_argv);
        assert_eq!(
            applied.target.as_deref(),
            Some(RETAIL),
            "the plan's client, by key"
        );
        assert!(applied.while_running, "the approved policy");
        assert_eq!(applied.if_match.as_ref(), Some(&plan.precondition.token));
        assert!(applied.force_revalidate);
        assert_eq!(applied.applying, Some(plan.plan_id));
        assert_eq!(
            applied.reason.as_deref(),
            Some("from the preview"),
            "the preview's"
        );
        assert_eq!(
            (applied.actor, applied.dry_run, applied.offline),
            (crate::model::Actor::Human, true, true),
            "apply's own"
        );
        let with_reason = applied_context(&ctx(&["omaddons", "apply", PLAN], Some("now")), &plan);
        assert_eq!(
            with_reason.reason.as_deref(),
            Some("now"),
            "apply's own --reason wins"
        );
    }

    #[test]
    fn needs_network_cases() {
        use crate::sources::SourceId;
        let intent = || StoredIntent::Install(install_intent(false));
        for (name, releases, want) in [
            (
                "direct_url",
                vec![release(
                    Some("https://github.com/o/foo/releases/download/v1/Foo.zip"),
                    None,
                )],
                true,
            ),
            (
                "deferred_asset",
                vec![release(None, Some(SourceId::Tukui))],
                true,
            ),
            ("local_zip", vec![release(None, None)], false),
            ("no_release", vec![], false),
            (
                "mixed",
                vec![
                    release(None, None),
                    release(Some("https://example.invalid/x.zip"), None),
                ],
                true,
            ),
        ] {
            assert_eq!(
                needs_network(&stored_plan(intent(), releases)),
                want,
                "{name}"
            );
        }
    }

    /// A refused flag's case: its name, the argv, whether the flag is `--while-running`
    /// (else `-t`), the fix's argv (`None`: prose) and `details.flag`.
    type FlagCase = (
        &'static str,
        &'static [&'static str],
        bool,
        Option<&'static [&'static str]>,
        &'static str,
    );

    #[test]
    fn refused_flags() {
        let cases: [FlagCase; 5] = [
            (
                "short_t",
                &["omaddons", "apply", PLAN, "-t", "retail"],
                false,
                Some(&["omaddons", "apply", PLAN]),
                "--target",
            ),
            (
                "long_equals",
                &["omaddons", "apply", PLAN, "--target=retail"],
                false,
                Some(&["omaddons", "apply", PLAN]),
                "--target",
            ),
            (
                "flavor",
                &["omaddons", "apply", PLAN, "--flavor", "retail"],
                false,
                Some(&["omaddons", "apply", PLAN]),
                "--target",
            ),
            (
                "cluster",
                &["omaddons", "apply", PLAN, "-vt", "retail"],
                false,
                None,
                "--target",
            ),
            (
                "while_running",
                &["omaddons", "apply", PLAN, "--while-running"],
                true,
                Some(&["omaddons", "apply", PLAN]),
                "--while-running",
            ),
        ];
        for (name, argv, while_running, fix, flag) in cases {
            let mut c = ctx(argv, None);
            if while_running {
                c.while_running = true;
            } else {
                c.target = Some("retail".into());
            }
            let error = refuse_own_flags(&c).unwrap_err();
            assert_eq!(error.code, ErrorCode::Usage, "{name}");
            assert_eq!(error.details, serde_json::json!({ "flag": flag }), "{name}");
            let want: Option<Vec<String>> =
                fix.map(|f| f.iter().map(|a| (*a).to_owned()).collect());
            assert_eq!(
                error.fix.argv().map(<[String]>::to_vec),
                want,
                "{name}: {:?}",
                error.fix
            );
        }
        assert!(refuse_own_flags(&ctx(&["omaddons", "apply", PLAN], None)).is_ok());
    }

    #[test]
    fn lease_timeout_is_locked_with_the_holder() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let paths = Paths::with_roots(&root, root.join("c"), root.join("s"), root.join("k"));
        std::fs::create_dir_all(paths.plans_dir()).unwrap();
        let id: PlanId = PLAN.parse().unwrap();
        let held = take_lease(&paths, &id, Duration::from_millis(50)).unwrap();
        let error = take_lease(&paths, &id, Duration::from_millis(50)).unwrap_err();
        drop(held);
        assert_eq!(
            (error.code, error.retryable),
            (ErrorCode::Locked, true),
            "{error:?}"
        );
        assert_eq!(error.details["lease"], PLAN, "{}", error.details);
        let holder = &error.details["holder"];
        assert_eq!(holder["pid"], std::process::id(), "{holder}");
        assert_eq!(holder["command"], "apply", "{holder}");
        assert!(
            holder["age_s"].as_i64().is_some_and(|age| age >= 0),
            "{holder}"
        );
        assert!(
            take_lease(&paths, &id, Duration::from_millis(50)).is_ok(),
            "released"
        );
    }

    #[test]
    fn lease_with_an_unreadable_holder_line_reports_null() {
        let id: PlanId = PLAN.parse().unwrap();
        let error = busy(
            &id,
            None,
            Duration::from_millis(50),
            CREATED.parse().unwrap(),
        );
        assert_eq!(
            error.details,
            serde_json::json!({ "lease": PLAN, "holder": null })
        );
        assert!(
            error.message.contains("another omaddons process"),
            "{}",
            error.message
        );
    }

    #[test]
    fn lease_age_never_negative() {
        let id: PlanId = PLAN.parse().unwrap();
        let now: Timestamp = CREATED.parse().unwrap();
        let ahead = HolderInfo {
            pid: 7,
            command: "apply".into(),
            started_at: now
                .checked_add(jiff::SignedDuration::from_hours(1))
                .unwrap(),
        };
        let error = busy(&id, Some(&ahead), Duration::from_millis(50), now);
        assert_eq!(error.details["holder"]["age_s"], 0, "{}", error.details);
    }

    #[test]
    fn storable_paths_are_the_storable_commands() {
        let intents: Vec<&str> = StoredIntent::PATHS.iter().map(|(p, _)| *p).collect();
        let commands: Vec<&str> = super::super::COMMANDS
            .iter()
            .filter(|c| c.stores_plan)
            .map(|c| c.name)
            .collect();
        assert_eq!(STORABLE_PATHS, intents, "the intent variants, in order");
        assert_eq!(
            STORABLE_PATHS, commands,
            "the COMMANDS rows that store a plan, in order"
        );
        assert_eq!(STORABLE_PATHS.len(), 11);
    }

    /// `node`, its `$ref` followed into `root`'s `$defs`.
    fn resolved<'a>(root: &'a Value, node: &'a Value) -> &'a Value {
        match node.get("$ref").and_then(Value::as_str) {
            Some(r) => {
                let name = r.strip_prefix("#/$defs/").unwrap();
                resolved(root, &root["$defs"][name])
            }
            None => node,
        }
    }

    #[test]
    fn apply_data_schema_is_one_complete_wrapper_per_storable_command() {
        let mut g = crate::contract::schema::generator();
        // Each row's own data schema, in row order: an independent copy of the table.
        let results: Vec<Value> = [
            g.subschema_for::<InstallData>(),
            g.subschema_for::<TargetsData>(),
            g.subschema_for::<TargetsData>(),
            g.subschema_for::<TargetsData>(),
            g.subschema_for::<AdoptData>(),
            g.subschema_for::<TargetsData>(),
            g.subschema_for::<TargetsData>(),
            g.subschema_for::<RemoveData>(),
            g.subschema_for::<UndoData>(),
            g.subschema_for::<BackupRestoreData>(),
            g.subschema_for::<BackupPruneData>(),
        ]
        .into_iter()
        .map(|s| s.to_value())
        .collect();
        let approval = g.subschema_for::<Approval>().to_value();
        let root = g.into_root_schema_for::<ApplyData>().to_value();
        let variants = root["oneOf"]
            .as_array()
            .unwrap_or_else(|| unreachable!("{root:#}"));
        assert_eq!(variants.len(), 11, "{root:#}");
        assert!(
            root.get("anyOf").is_none(),
            "a union is oneOf, never anyOf: {root:#}"
        );
        let fields = ["applied", "approved", "outcomes", "plan_id", "result"];
        for ((variant, path), result) in variants.iter().zip(STORABLE_PATHS).zip(&results) {
            let wrapper = resolved(&root, variant);
            let props = wrapper["properties"].as_object().unwrap();
            let mut keys: Vec<&str> = props.keys().map(String::as_str).collect();
            keys.sort_unstable();
            assert_eq!(keys, fields, "{path}: {wrapper:#}");
            let mut required: Vec<&str> = wrapper["required"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(Value::as_str)
                .collect();
            required.sort_unstable();
            assert_eq!(
                required, fields,
                "{path}: every field is always present: {wrapper:#}"
            );
            let applied = resolved(&root, &props["applied"]);
            let command = &applied["properties"]["command"];
            assert_eq!(
                (&command["type"], &command["const"], command.get("enum")),
                (&serde_json::json!("string"), &serde_json::json!(path), None),
                "{path}: one value, as a const: {applied:#}"
            );
            assert!(result["$ref"].is_string(), "{path}: {result}");
            assert_eq!(
                props["result"]["$ref"], result["$ref"],
                "{path}: its command's data: {}",
                props["result"]
            );
            assert!(
                props["approved"]
                    .to_string()
                    .contains(&approval.to_string()),
                "{path}: the approval or null: {}",
                props["approved"]
            );
        }
    }

    #[test]
    fn apply_data_keys_render_in_order() {
        let data = ApplyData::BackupRestore(Wrapper {
            plan_id: "01M9P0000000000000000000PA".parse().unwrap(),
            applied: Applied {
                command: tag::BackupRestore::BackupRestore,
                display_argv: vec!["omaddons".into(), "backup".into(), "restore".into()],
            },
            approved: None,
            result: restore(false, Some(tx(1))),
            outcomes: outcomes_of(&restore(false, Some(tx(1)))),
        });
        let json = serde_json::to_string(&data).unwrap();
        let at = |key: &str| {
            json.find(key)
                .unwrap_or_else(|| unreachable!("{key} in {json}"))
        };
        let order = [
            at(r#"{"plan_id":"01M9P"#),
            at(r#","applied":{"command":"backup restore","display_argv":["#),
            at(r#","approved":null,"result":{"#),
            json.rfind(r#","outcomes":[{"target":"retail""#).unwrap(),
        ];
        assert_eq!(order[0], 0, "{json}");
        assert!(order.windows(2).all(|w| w[0] < w[1]), "{order:?}: {json}");
    }

    #[test]
    fn target_outcomes_cases() {
        use OutcomeStatus::{Committed, Failed, Stale, Unchanged};
        let stale = Some(ErrorCode::PlanStale);
        let network = Some(ErrorCode::Network);
        let ok = |status| item(status, None);
        let cases: Vec<(&str, Value, Vec<TargetOutcome>, Vec<TargetOutcome>)> = vec![
            {
                let data = targets(
                    false,
                    vec![on(
                        "retail",
                        RETAIL,
                        Some(tx(1)),
                        None,
                        vec![ok(ItemStatus::Updated)],
                    )],
                );
                (
                    "targets_committed",
                    serde_json::to_value(&data).unwrap(),
                    outcomes_of(&data),
                    vec![out("retail", RETAIL, Committed, Some(tx(1)), None)],
                )
            },
            {
                let data = targets(false, vec![on("retail", RETAIL, None, stale, vec![])]);
                (
                    "targets_stale",
                    serde_json::to_value(&data).unwrap(),
                    outcomes_of(&data),
                    vec![out("retail", RETAIL, Stale, None, stale)],
                )
            },
            {
                let data = targets(false, vec![on("retail", RETAIL, None, network, vec![])]);
                (
                    "targets_failed_by_error",
                    serde_json::to_value(&data).unwrap(),
                    outcomes_of(&data),
                    vec![out("retail", RETAIL, Failed, None, network)],
                )
            },
            {
                let data = targets(
                    false,
                    vec![on(
                        "retail",
                        RETAIL,
                        None,
                        None,
                        vec![
                            ok(ItemStatus::Skipped),
                            item(ItemStatus::Failed, Some(ErrorCode::HashMismatch)),
                            item(ItemStatus::Failed, network),
                        ],
                    )],
                );
                (
                    "targets_failed_by_item",
                    serde_json::to_value(&data).unwrap(),
                    outcomes_of(&data),
                    vec![out(
                        "retail",
                        RETAIL,
                        Failed,
                        None,
                        Some(ErrorCode::HashMismatch),
                    )],
                )
            },
            {
                let data = targets(
                    false,
                    vec![on(
                        "retail",
                        RETAIL,
                        None,
                        None,
                        vec![ok(ItemStatus::Unchanged), ok(ItemStatus::Skipped)],
                    )],
                );
                (
                    "targets_unchanged",
                    serde_json::to_value(&data).unwrap(),
                    outcomes_of(&data),
                    vec![out("retail", RETAIL, Unchanged, None, None)],
                )
            },
            {
                let data = targets(
                    true,
                    vec![
                        on("retail", RETAIL, None, None, vec![ok(ItemStatus::Updated)]),
                        on("classic", CLASSIC, None, stale, vec![]),
                    ],
                );
                (
                    "targets_dry_run",
                    serde_json::to_value(&data).unwrap(),
                    outcomes_of(&data),
                    vec![
                        out("classic", CLASSIC, Stale, None, stale),
                        out("retail", RETAIL, Unchanged, None, None),
                    ],
                )
            },
            {
                let data = AdoptData::Adopted(Box::new(targets(
                    false,
                    vec![on(
                        "retail",
                        RETAIL,
                        Some(tx(2)),
                        None,
                        vec![ok(ItemStatus::Adopted)],
                    )],
                )));
                (
                    "adopt_committed",
                    serde_json::to_value(&data).unwrap(),
                    outcomes_of(&data),
                    vec![out("retail", RETAIL, Committed, Some(tx(2)), None)],
                )
            },
            {
                let data = install(
                    false,
                    Some(tx(3)),
                    vec![
                        install_item(ItemStatus::Installed, None),
                        install_item(ItemStatus::Failed, network),
                    ],
                );
                (
                    "install_committed",
                    serde_json::to_value(&data).unwrap(),
                    outcomes_of(&data),
                    vec![out("retail", RETAIL, Committed, Some(tx(3)), None)],
                )
            },
            {
                let data = install(false, None, vec![install_item(ItemStatus::Failed, network)]);
                (
                    "install_failed_item",
                    serde_json::to_value(&data).unwrap(),
                    outcomes_of(&data),
                    vec![out("retail", RETAIL, Failed, None, network)],
                )
            },
            {
                let data = install(false, None, vec![install_item(ItemStatus::Failed, stale)]);
                (
                    "install_stale_item",
                    serde_json::to_value(&data).unwrap(),
                    outcomes_of(&data),
                    vec![out("retail", RETAIL, Stale, None, stale)],
                )
            },
            {
                let data = install(false, None, vec![install_item(ItemStatus::Unchanged, None)]);
                (
                    "install_unchanged",
                    serde_json::to_value(&data).unwrap(),
                    outcomes_of(&data),
                    vec![out("retail", RETAIL, Unchanged, None, None)],
                )
            },
            {
                let data = install(true, None, vec![install_item(ItemStatus::Installed, None)]);
                (
                    "install_dry_run",
                    serde_json::to_value(&data).unwrap(),
                    outcomes_of(&data),
                    vec![out("retail", RETAIL, Unchanged, None, None)],
                )
            },
            {
                let data = remove(Some(tx(4)), None);
                (
                    "remove_committed",
                    serde_json::to_value(&data).unwrap(),
                    outcomes_of(&data),
                    vec![out("retail", RETAIL, Committed, Some(tx(4)), None)],
                )
            },
            {
                let data = remove(None, stale);
                (
                    "remove_stale_item",
                    serde_json::to_value(&data).unwrap(),
                    outcomes_of(&data),
                    vec![out("retail", RETAIL, Stale, None, stale)],
                )
            },
            {
                let data = undo(
                    false,
                    &[
                        ("retail", RETAIL, Some(tx(5)), None),
                        ("classic", CLASSIC, None, network),
                    ],
                );
                (
                    "undo_two_targets",
                    serde_json::to_value(&data).unwrap(),
                    outcomes_of(&data),
                    vec![
                        out("classic", CLASSIC, Failed, None, network),
                        out("retail", RETAIL, Committed, Some(tx(5)), None),
                    ],
                )
            },
            {
                let data = undo(
                    false,
                    &[
                        ("retail", RETAIL, Some(tx(6)), None),
                        ("retail", RETAIL, Some(tx(7)), None),
                    ],
                );
                (
                    "undo_last_tx_on_a_target",
                    serde_json::to_value(&data).unwrap(),
                    outcomes_of(&data),
                    vec![out("retail", RETAIL, Committed, Some(tx(7)), None)],
                )
            },
            {
                let data = undo(false, &[("retail", RETAIL, None, stale)]);
                (
                    "undo_stale",
                    serde_json::to_value(&data).unwrap(),
                    outcomes_of(&data),
                    vec![out("retail", RETAIL, Stale, None, stale)],
                )
            },
            {
                let data = undo(true, &[("retail", RETAIL, None, None)]);
                (
                    "undo_dry_run",
                    serde_json::to_value(&data).unwrap(),
                    outcomes_of(&data),
                    vec![out("retail", RETAIL, Unchanged, None, None)],
                )
            },
            {
                let data = restore(false, Some(tx(8)));
                (
                    "restore_committed",
                    serde_json::to_value(&data).unwrap(),
                    outcomes_of(&data),
                    vec![out("retail", RETAIL, Committed, Some(tx(8)), None)],
                )
            },
            {
                let data = restore(true, None);
                (
                    "restore_dry_run",
                    serde_json::to_value(&data).unwrap(),
                    outcomes_of(&data),
                    vec![out("retail", RETAIL, Unchanged, None, None)],
                )
            },
            {
                let data = prune(false, &[("retail", RETAIL, 2, None)]);
                (
                    "prune_with_deletions",
                    serde_json::to_value(&data).unwrap(),
                    outcomes_of(&data),
                    vec![out("retail", RETAIL, Committed, None, None)],
                )
            },
            {
                let data = prune(false, &[("retail", RETAIL, 0, None)]);
                (
                    "prune_nothing",
                    serde_json::to_value(&data).unwrap(),
                    outcomes_of(&data),
                    vec![out("retail", RETAIL, Unchanged, None, None)],
                )
            },
            {
                let data = prune(
                    false,
                    &[
                        ("retail", RETAIL, 0, Some(ErrorCode::Io)),
                        ("classic", CLASSIC, 0, stale),
                    ],
                );
                (
                    "prune_error",
                    serde_json::to_value(&data).unwrap(),
                    outcomes_of(&data),
                    vec![
                        out("classic", CLASSIC, Stale, None, stale),
                        out("retail", RETAIL, Failed, None, Some(ErrorCode::Io)),
                    ],
                )
            },
            {
                let data = prune(true, &[("retail", RETAIL, 2, None)]);
                (
                    "prune_dry_run",
                    serde_json::to_value(&data).unwrap(),
                    outcomes_of(&data),
                    vec![out("retail", RETAIL, Unchanged, None, None)],
                )
            },
            {
                let data = AdoptData::Report(AdoptReport {
                    verify: None,
                    targets: Vec::new(),
                    sources_skipped: Vec::new(),
                });
                (
                    "adopt_report_has_no_entries",
                    serde_json::to_value(&data).unwrap(),
                    outcomes_of(&data),
                    vec![],
                )
            },
            {
                let data = targets(
                    false,
                    vec![
                        on("retail", RETAIL, Some(tx(9)), None, vec![]),
                        on("classic", CLASSIC, None, None, vec![]),
                    ],
                );
                (
                    "sorted_by_target_id",
                    serde_json::to_value(&data).unwrap(),
                    outcomes_of(&data),
                    vec![
                        out("classic", CLASSIC, Unchanged, None, None),
                        out("retail", RETAIL, Committed, Some(tx(9)), None),
                    ],
                )
            },
        ];
        for (name, input, got, want) in cases {
            assert_eq!(got, want, "{name}: from {input:#}");
        }
    }
}
