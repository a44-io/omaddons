//! The delegation seam (M4-D37): a `doctor fix` has the engine's own recovery settle the
//! journals its plan disclosed, under the lock it holds, after its token was compared and
//! before its repair. The runtime owns where the call sits in that order; F2 supplies the
//! delegate that recovers ([`JournalDelegate`]), and the fixer whose plan delegates
//! (`doctor::fixers::recover`).

use std::collections::BTreeSet;
use std::path::Path;

use schemars::JsonSchema;
#[cfg(feature = "schemas")]
use schemars::{Schema, SchemaGenerator, json_schema};
use serde::Serialize;

use super::fix::{PlannedPrune, PlannedRecovery};
use super::report::{PruneDone, PruneOutcome};
use crate::discover::registry::same_dir;
use crate::discover::running::{RunningScan, detect_running};
use crate::engine::journal::{self, Disclosed, JournalFamily, StopReason, TxOutcome, TxResult};
use crate::engine::retention;
use crate::error::{Error, ErrorCode, Result, Suggestion, Warning, WarningCode};
use crate::fsx::GlobalLockGuard;
use crate::model::{Actor, Target, TxId};
use crate::state::paths::ClientPaths;
use crate::state::paths::Paths;

/// What runs a plan's disclosed recoveries.
pub trait RecoveryDelegate {
    /// Runs exactly `disclosed`, in order, under `lock`; never anything else. `targets`
    /// are the clients the fresh view under the lock discovered: only a discovered client
    /// whose game is not running gets SavedVariables written.
    fn recover(
        &self,
        lock: &GlobalLockGuard,
        disclosed: &[PlannedRecovery],
        targets: &[Target],
    ) -> Result<DelegationReport>;

    /// Has engine retention prune each of `prunes` under `lock`, exactly the planned
    /// transactions it still prunes; one outcome per client, in order, a failure kept
    /// with its error code and the next client still pruned.
    fn retain(&self, lock: &GlobalLockGuard, prunes: &[PlannedPrune]) -> Vec<PruneDone>;
}

/// `prune`'s failed outcome, with `error`'s code.
fn prune_failed(prune: &PlannedPrune, error: &Error) -> PruneDone {
    PruneDone {
        fm: prune.fm,
        target: prune.target.clone(),
        key: prune.key,
        outcome: PruneOutcome::Failed,
        pruned: Vec::new(),
        reason: Some(error.code.wire().to_owned()),
    }
}

/// What a delegation did: one outcome per disclosed recovery, in disclosure order, and the
/// warnings recovery gave, each delegated roll in `delegated_recovery` form.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DelegationReport {
    pub delegated: Vec<DelegatedOutcome>,
    pub warnings: Vec<Warning>,
}

/// How one delegated recovery ended (a fix report's `delegated[]`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct DelegatedOutcome {
    pub fm: &'static str,
    pub tx: TxId,
    pub family: JournalFamily,
    pub outcome: RecoveryOutcome,
    /// Present only when the recovery waits or failed: its stop's `details.reason`, else
    /// its error code, or `not_reached` when recovery ended before reaching it.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "delegated_reason_schema")]
    pub reason: Option<String>,
}

/// The `reason` of a disclosed recovery recovery never reached.
pub const NOT_REACHED: &str = "not_reached";

/// Every `reason` a delegated outcome gives: the stop reasons, the error codes, and
/// [`NOT_REACHED`].
pub fn delegated_reasons() -> Vec<String> {
    let mut out: Vec<String> = StopReason::ALL
        .iter()
        .map(|r| r.as_str().to_owned())
        .chain(ErrorCode::ALL.iter().map(|c| c.wire().to_owned()))
        .chain(std::iter::once(NOT_REACHED.to_owned()))
        .collect();
    out.sort();
    out.dedup();
    out
}

#[cfg(feature = "schemas")]
fn delegated_reason_schema(_: &mut SchemaGenerator) -> Schema {
    json_schema!({ "type": "string", "enum": delegated_reasons() })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryOutcome {
    RolledBack,
    RolledForward,
    /// It waits for something no repair can supply (a game to exit).
    Waiting,
    Failed,
}

/// The delegate of a run that must never recover (the doctor's own unit tests).
pub struct NoDelegate;

impl RecoveryDelegate for NoDelegate {
    fn recover(
        &self,
        _: &GlobalLockGuard,
        disclosed: &[PlannedRecovery],
        _: &[Target],
    ) -> Result<DelegationReport> {
        if disclosed.is_empty() {
            return Ok(DelegationReport::default());
        }
        Err(Error::internal(format!(
            "this build has no recovery delegate, so the {} recoveries the plan disclosed were not run; nothing was changed",
            disclosed.len()
        )))
    }

    fn retain(&self, _: &GlobalLockGuard, prunes: &[PlannedPrune]) -> Vec<PruneDone> {
        let error = Error::internal("this build has no retention delegate; nothing was pruned");
        prunes.iter().map(|p| prune_failed(p, &error)).collect()
    }
}

/// The real delegate: the engine's recovery (`journal::recover_disclosed`), restricted to
/// the disclosed transactions, with SavedVariables written only where the engine's rule
/// lets a change write them.
pub struct JournalDelegate<'a> {
    pub paths: &'a Paths,
    pub actor: Actor,
}

impl RecoveryDelegate for JournalDelegate<'_> {
    fn recover(
        &self,
        lock: &GlobalLockGuard,
        disclosed: &[PlannedRecovery],
        targets: &[Target],
    ) -> Result<DelegationReport> {
        if disclosed.is_empty() {
            return Ok(DelegationReport::default());
        }
        // One fresh look at the process table, taken now, under the lock.
        let scan = detect_running(targets);
        let writable = |dir: &Path| sv_writable(targets, &scan, dir);
        let only: BTreeSet<TxId> = disclosed.iter().map(|r| r.tx).collect();
        let found = journal::recover_disclosed(
            lock,
            self.paths,
            &writable,
            "doctor fix",
            self.actor,
            Some(&only),
        );
        Ok(report_of(disclosed, found))
    }

    /// Each client through `retention::prune_selected`, with a fenced write table of its
    /// own as the daily pass builds one, at the system clock's now, as retention's ages
    /// are told.
    fn retain(&self, lock: &GlobalLockGuard, prunes: &[PlannedPrune]) -> Vec<PruneDone> {
        let now = jiff::Timestamp::now();
        prunes
            .iter()
            .map(|p| {
                let one = || -> Result<Vec<TxId>> {
                    let scopes = crate::engine::fence::table(
                        self.paths,
                        std::slice::from_ref(&p.client_dir),
                        None,
                    )?;
                    retention::prune_selected(
                        self.paths,
                        &scopes,
                        p.key,
                        &ClientPaths::new(&p.client_dir),
                        &p.txs,
                        &p.limits,
                        now,
                        lock,
                    )
                };
                match one() {
                    Ok(pruned) => PruneDone {
                        fm: p.fm,
                        target: p.target.clone(),
                        key: p.key,
                        outcome: PruneOutcome::Pruned,
                        pruned,
                        reason: None,
                    },
                    Err(error) => prune_failed(p, &error),
                }
            })
            .collect()
    }
}

/// Whether the SavedVariables of the client at `dir` may be written now: the engine's rule
/// (`engine::tx::sv_writable`) from the doctor's view. A client the view discovered (the
/// same directory, also through a symlink) whose game the scan does not see running; with
/// the process table unreadable, none.
pub fn sv_writable(targets: &[Target], scan: &RunningScan, dir: &Path) -> bool {
    scan.unavailable.is_none()
        && targets
            .iter()
            .find(|t| same_dir(&t.client_dir, dir))
            .is_some_and(|t| !scan.clients.contains_key(&t.client_dir))
}

/// The report of a delegation: one outcome per disclosed recovery (in disclosure order),
/// then the warnings, each reached transaction's in its order with a roll's
/// `journal_recovered` in `delegated_recovery` form, then the rest as recovery gave them.
fn report_of(disclosed: &[PlannedRecovery], found: Disclosed) -> DelegationReport {
    let delegated = disclosed
        .iter()
        .map(|planned| {
            // ubs:ignore — comparing transaction ids (public ULIDs), not secrets
            let reached = found.outcomes.iter().find(|o| o.tx == planned.tx);
            outcome_for(planned, reached, found.error.as_ref())
        })
        .collect();
    let mut warnings: Vec<Warning> = Vec::new();
    for reached in &found.outcomes {
        warnings.extend(
            reached
                .warnings
                .iter()
                .map(|w| delegated_warning(reached, w)),
        );
    }
    warnings.extend(found.warnings);
    DelegationReport {
        delegated,
        warnings,
    }
}

/// One disclosed recovery's outcome, from what recovery did with it (`reached`), or, when
/// it never reached it, from what stopped the call.
pub fn outcome_for(
    planned: &PlannedRecovery,
    reached: Option<&TxOutcome>,
    call_error: Option<&Error>,
) -> DelegatedOutcome {
    // A stop's reason when it is one of recovery's, else the error's code: the closed
    // vocabulary `delegated_reasons` publishes.
    let reason_of = |code: ErrorCode, reason: Option<&str>| {
        Some(
            reason
                .and_then(StopReason::parse)
                .map_or_else(|| code.wire().to_owned(), |r| r.as_str().to_owned()),
        )
    };
    let (outcome, reason) = match reached.map(|o| &o.result) {
        Some(TxResult::RolledBack | TxResult::Pruned) => (RecoveryOutcome::RolledBack, None),
        Some(TxResult::RolledForward) => (RecoveryOutcome::RolledForward, None),
        Some(TxResult::Stopped {
            code: ErrorCode::GameRunning,
            reason,
        }) => (
            RecoveryOutcome::Waiting,
            Some(
                reason
                    .as_deref()
                    .and_then(StopReason::parse)
                    .unwrap_or(StopReason::SavedVariables)
                    .as_str()
                    .to_owned(),
            ),
        ),
        Some(TxResult::Stopped { code, reason }) => {
            (RecoveryOutcome::Failed, reason_of(*code, reason.as_deref()))
        }
        None => {
            let reason = call_error.map_or_else(
                || Some(NOT_REACHED.to_owned()),
                |error| reason_of(error.code, error.details["reason"].as_str()),
            );
            (RecoveryOutcome::Failed, reason)
        }
    };
    DelegatedOutcome {
        fm: planned.fm,
        tx: planned.tx,
        family: planned.family,
        outcome,
        reason,
    }
}

/// `warning`, which recovering `reached` gave: its `journal_recovered` becomes one
/// `delegated_recovery` with the same text prefixed `doctor fix: `, whose fix undoes a
/// roll-forward by its transaction id or, for a rollback, shows history; anything else
/// passes through.
pub fn delegated_warning(reached: &TxOutcome, warning: &Warning) -> Warning {
    if warning.code != WarningCode::JournalRecovered {
        return warning.clone();
    }
    let fix = match reached.result {
        TxResult::RolledForward => Suggestion::command([
            "omaddons".to_owned(),
            "undo".to_owned(),
            reached.tx.to_string(),
        ]),
        _ => Suggestion::command(["omaddons", "history", "--limit", "5"]),
    };
    Warning::new(
        WarningCode::DelegatedRecovery,
        format!("doctor fix: {}", warning.message),
    )
    .with_fix(fix)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use serde_json::json;

    use super::*;
    use crate::discover::running::RunningInfo;
    use crate::engine::journal::{Direction, Phase};
    use crate::engine::resource::ResourceSet;
    use crate::fsx::GlobalLock;

    const PENDING: &str = "fm-concurrency_primitives-journal-pending";

    fn tx() -> TxId {
        "01JB8Q2VA00000000000000009".parse().unwrap()
    }

    fn planned() -> PlannedRecovery {
        PlannedRecovery {
            fm: PENDING,
            tx: tx(),
            family: JournalFamily::Folder,
            phase: Phase::Prepared,
            repair_phase: None,
            direction: Direction::Back,
            protected: ResourceSet::new(),
            disclosed: crate::engine::repair_action::Delegation {
                tx: tx(),
                family: JournalFamily::Folder,
                phase: crate::engine::repair_action::JournalPhase::Prepared,
                direction: crate::engine::repair_action::Direction::RollBack,
            },
        }
    }

    fn reached(result: TxResult) -> TxOutcome {
        TxOutcome {
            tx: tx(),
            family: JournalFamily::Folder,
            result,
            warnings: Vec::new(),
        }
    }

    #[test]
    fn no_delegate_refuses_a_non_empty_set() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::with_roots(
            dir.path(),
            dir.path().join("c"),
            dir.path().join("s"),
            dir.path().join("k"),
        );
        let lock = GlobalLock::acquire(&paths, "test").unwrap();
        assert_eq!(
            NoDelegate.recover(&lock, &[], &[]).unwrap(),
            DelegationReport::default()
        );
        let err = NoDelegate.recover(&lock, &[planned()], &[]).unwrap_err();
        assert_eq!(err.code, ErrorCode::Internal, "{}", err.message);
    }

    #[test]
    fn recovery_outcome_wire_names() {
        let names: Vec<serde_json::Value> = [
            RecoveryOutcome::RolledBack,
            RecoveryOutcome::RolledForward,
            RecoveryOutcome::Waiting,
            RecoveryOutcome::Failed,
        ]
        .iter()
        .map(|o| serde_json::to_value(o).unwrap())
        .collect();
        assert_eq!(
            names,
            ["rolled_back", "rolled_forward", "waiting", "failed"].map(serde_json::Value::from)
        );
    }

    /// One case per row of F2 s1's outcome table (§3.3).
    #[test]
    fn outcome_for_each_recovery_result() {
        let stopped = |code, reason: Option<&str>| TxResult::Stopped {
            code,
            reason: reason.map(str::to_owned),
        };
        let io = Error::new(ErrorCode::Io, "disk full", Suggestion::text("free space"));
        let unattributed =
            Error::internal("listing failed").details(json!({"reason": "unattributed"}));
        type Case<'a> = (
            &'a str,
            Option<TxResult>,
            Option<&'a Error>,
            RecoveryOutcome,
            Option<&'a str>,
        );
        let cases: Vec<Case<'_>> = vec![
            (
                "rolled_back",
                Some(TxResult::RolledBack),
                None,
                RecoveryOutcome::RolledBack,
                None,
            ),
            (
                "rolled_forward",
                Some(TxResult::RolledForward),
                None,
                RecoveryOutcome::RolledForward,
                None,
            ),
            (
                "pruned_is_a_rollback",
                Some(TxResult::Pruned),
                None,
                RecoveryOutcome::RolledBack,
                None,
            ),
            (
                "game_running_waits",
                Some(stopped(ErrorCode::GameRunning, Some("saved_variables"))),
                None,
                RecoveryOutcome::Waiting,
                Some("saved_variables"),
            ),
            (
                "game_running_without_reason_waits_for_saved_variables",
                Some(stopped(ErrorCode::GameRunning, None)),
                None,
                RecoveryOutcome::Waiting,
                Some("saved_variables"),
            ),
            (
                "stopped_with_reason",
                Some(stopped(ErrorCode::ScopeDenied, Some("marker_mismatch"))),
                None,
                RecoveryOutcome::Failed,
                Some("marker_mismatch"),
            ),
            (
                "stopped_with_a_reason_recovery_never_gives_uses_the_code",
                Some(stopped(ErrorCode::ScopeDenied, Some("formatted text"))),
                None,
                RecoveryOutcome::Failed,
                Some("scope_denied"),
            ),
            (
                "stopped_without_reason_uses_the_code",
                Some(stopped(ErrorCode::Io, None)),
                None,
                RecoveryOutcome::Failed,
                Some("io"),
            ),
            (
                "not_reached_is_failed",
                None,
                Some(&unattributed),
                RecoveryOutcome::Failed,
                Some("unattributed"),
            ),
            (
                "not_reached_without_reason_uses_the_code",
                None,
                Some(&io),
                RecoveryOutcome::Failed,
                Some("io"),
            ),
            (
                "not_reached_by_a_call_that_ended",
                None,
                None,
                RecoveryOutcome::Failed,
                Some("not_reached"),
            ),
        ];
        let published = delegated_reasons();
        for (case, result, error, outcome, reason) in cases {
            let reached = result.map(reached);
            let got = outcome_for(&planned(), reached.as_ref(), error);
            assert!(
                got.reason.as_ref().is_none_or(|r| published.contains(r)),
                "{case}: {got:?} gives a reason the schema does not list"
            );
            assert_eq!(
                (got.outcome, got.reason.as_deref()),
                (outcome, reason),
                "{case}: {reached:?} {error:?} → {got:?}"
            );
            assert_eq!(
                (got.fm, got.tx, got.family),
                (PENDING, tx(), JournalFamily::Folder),
                "{case}"
            );
        }
    }

    #[test]
    fn journal_recovered_becomes_delegated_recovery() {
        let recovered = Warning::new(
            WarningCode::JournalRecovered,
            format!("transaction {} on target K was interrupted; recovery completed it (rolled forward)", tx()),
        )
        .with_fix(Suggestion::command(["omaddons", "history", "--limit", "5"]));
        let forward = delegated_warning(&reached(TxResult::RolledForward), &recovered);
        assert_eq!(forward.code, WarningCode::DelegatedRecovery, "{forward:?}");
        assert_eq!(
            forward.message,
            format!("doctor fix: {}", recovered.message)
        );
        let wire = serde_json::to_value(&forward).unwrap();
        assert_eq!(
            wire["fix_argv"],
            json!(["omaddons", "undo", tx().to_string()]),
            "{wire}"
        );
        let back = delegated_warning(&reached(TxResult::RolledBack), &recovered);
        let wire = serde_json::to_value(&back).unwrap();
        assert_eq!(
            wire["fix_argv"],
            json!(["omaddons", "history", "--limit", "5"]),
            "{wire}"
        );
        let deferred = Warning::new(WarningCode::SavedVariablesDeferred, "left as they are")
            .with_fix(Suggestion::text("quit the game"));
        assert_eq!(
            delegated_warning(&reached(TxResult::RolledForward), &deferred),
            deferred,
            "other warnings pass through"
        );
    }

    #[test]
    fn report_keeps_disclosure_order_and_passes_other_warnings_through() {
        let first = planned();
        let second = PlannedRecovery {
            tx: "01JB8Q2VA0000000000000000A".parse().unwrap(),
            ..planned()
        };
        let recovered = Warning::new(WarningCode::JournalRecovered, "rolled back");
        let pending = Warning::new(WarningCode::JournalPending, "another client's");
        let found = Disclosed {
            outcomes: vec![TxOutcome {
                warnings: vec![recovered],
                ..reached(TxResult::RolledBack)
            }],
            warnings: vec![pending.clone()],
            error: None,
        };
        let report = report_of(&[first, second.clone()], found);
        assert_eq!(
            report
                .delegated
                .iter()
                .map(|d| (d.tx, d.outcome))
                .collect::<Vec<_>>(),
            [
                (tx(), RecoveryOutcome::RolledBack),
                (second.tx, RecoveryOutcome::Failed)
            ]
        );
        assert_eq!(
            report.warnings.iter().map(|w| w.code).collect::<Vec<_>>(),
            [WarningCode::DelegatedRecovery, WarningCode::JournalPending]
        );
        assert_eq!(report.warnings[1], pending);
    }

    /// One serialized item per outcome; `reason` only where one waits or failed.
    #[test]
    fn delegated_items_serialize() {
        let item = |outcome, reason: Option<&str>, family| DelegatedOutcome {
            fm: PENDING,
            tx: tx(),
            family,
            outcome,
            reason: reason.map(str::to_owned),
        };
        let items = [
            item(RecoveryOutcome::RolledBack, None, JournalFamily::Folder),
            item(RecoveryOutcome::RolledForward, None, JournalFamily::Repair),
            item(
                RecoveryOutcome::Waiting,
                Some("saved_variables"),
                JournalFamily::Folder,
            ),
            item(RecoveryOutcome::Failed, Some("io"), JournalFamily::Folder),
        ];
        insta::assert_json_snapshot!(items);
    }

    /// `sv_writable` is the engine's rule: discovered and idle only.
    #[test]
    fn sv_writable_follows_the_engine_rule() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let client = root.join("_retail_");
        std::fs::create_dir_all(&client).unwrap();
        let link = root.join("linked");
        std::os::unix::fs::symlink(&client, &link).unwrap();
        let target = crate::engine::testing::target(&client);
        let idle = RunningScan::default();
        let running = RunningScan {
            clients: BTreeMap::from([(client.clone(), Vec::<RunningInfo>::new())]),
            unavailable: None,
        };
        let unreadable = RunningScan {
            clients: BTreeMap::new(),
            unavailable: Some("/proc could not be read".to_owned()),
        };
        let other: PathBuf = root.join("_classic_");
        type Case<'a> = (&'a str, &'a [Target], &'a RunningScan, &'a Path, bool);
        let targets = [target];
        let cases: [Case<'_>; 5] = [
            ("discovered_and_idle", &targets, &idle, &client, true),
            ("discovered_and_running", &targets, &running, &client, false),
            ("undiscovered", &targets, &idle, &other, false),
            (
                "process_table_unreadable",
                &targets,
                &unreadable,
                &client,
                false,
            ),
            ("reached_through_a_symlink", &targets, &idle, &link, true),
        ];
        for (case, targets, scan, dir, want) in cases {
            assert_eq!(
                sv_writable(targets, scan, dir),
                want,
                "{case}: {dir:?} {scan:?}"
            );
        }
    }
}
