//! What `omaddons apply` executes a stored plan with (M4-D23), before the command around
//! it exists: a plan's typed intent run by its command's own `run`, the typed outcome
//! that returns ([`StorableOutcome`]), the wrapper `apply`'s data is ([`ApplyData`]), and
//! the per-target report ([`TargetOutcome`], M4-D61). Nothing here parses an argv: an
//! intent becomes its command's arguments by conversion (`From<&XIntent> for XArgs`, in
//! `intent.rs`), and an intent can name no command but a storable one.

use std::borrow::Cow;
use std::io::Write;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::Serialize;

use super::adopt::{self, AdoptArgs, AdoptData, AdoptOutcome};
use super::approval::Approval;
use super::backup::{
    self, BackupPruneArgs, BackupPruneData, BackupPruneOutcome, BackupRestoreArgs,
    BackupRestoreData, BackupRestoreOutcome,
};
use super::copy::{self, CopyArgs, CopyOutcome};
use super::install::{self, InstallArgs, InstallData, InstallOutcome};
use super::intent::StoredIntent;
use super::outcome::{ItemStatus, TargetsData};
use super::pin::{self, PinArgs, PinOutcome, UnpinArgs};
use super::remove::{self, RemoveArgs, RemoveData, RemoveOutcome, RemoveStatus};
use super::sync::{self, SyncArgs, SyncOutcome};
use super::undo::{self, UndoArgs, UndoData, UndoOutcome, UndoTxStatus};
use super::update::{self, UpdateArgs, UpdateOutcome};
use crate::cli::Context;
use crate::error::{CommandHint, Error, ErrorCode, Result, Warning};
use crate::model::{PlanId, TargetKey, TxId};
use crate::output::{Outcome, Style};

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
    /// there, else its first error.
    fn facts(&self) -> Vec<Facts> {
        let mut facts: Vec<Facts> = Vec::new();
        for t in &self.transactions {
            let at = match facts.iter().position(|f| f.key == Some(t.target.key)) {
                Some(at) => at,
                None => {
                    facts.push(Facts {
                        target: t.target.id.clone(),
                        key: Some(t.target.key),
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
                target: history_target(id, k),
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
            reverted: Reverted {
                tx: tx(50),
                kind: TxKind::Install,
                at: "2026-10-29T12:00:00Z".parse().unwrap(),
                target: history_target("retail", RETAIL),
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
                    busy: Vec::new(),
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
