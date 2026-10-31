//! The delegation seam (M4-D37): a `doctor fix` has the engine's own recovery settle the
//! journals its plan disclosed, under the lock it holds, after its token was compared and
//! before its repair. The runtime owns where the call sits in that order; F2 supplies the
//! delegate that recovers, and the fixer whose plan delegates.

use schemars::JsonSchema;
use serde::Serialize;

use super::fix::PlannedRecovery;
use crate::error::{Error, Result};
use crate::fsx::GlobalLockGuard;
use crate::model::TxId;

/// What runs a plan's disclosed recoveries.
pub trait RecoveryDelegate {
    /// Runs exactly `disclosed`, in order, under `lock`; never anything else.
    fn recover(
        &self,
        lock: &GlobalLockGuard,
        disclosed: &[PlannedRecovery],
    ) -> Result<Vec<DelegatedOutcome>>;
}

/// How one delegated recovery ended (a fix report's `delegated[]`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct DelegatedOutcome {
    pub fm: &'static str,
    pub tx: TxId,
    pub outcome: RecoveryOutcome,
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

/// The delegate of a build no fixer delegates in: nothing returns `Fix::Delegate` before
/// F2, which installs the real one.
pub struct NoDelegate;

impl RecoveryDelegate for NoDelegate {
    fn recover(
        &self,
        _: &GlobalLockGuard,
        disclosed: &[PlannedRecovery],
    ) -> Result<Vec<DelegatedOutcome>> {
        if disclosed.is_empty() {
            return Ok(Vec::new());
        }
        Err(Error::internal(format!(
            "this build has no recovery delegate, so the {} recoveries the plan disclosed were not run; nothing was changed",
            disclosed.len()
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::journal::{Direction, JournalFamily, Phase};
    use crate::engine::resource::ResourceSet;
    use crate::error::ErrorCode;
    use crate::fsx::GlobalLock;
    use crate::state::paths::Paths;

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
        assert_eq!(NoDelegate.recover(&lock, &[]).unwrap(), []);
        let disclosed = PlannedRecovery {
            fm: "fm-concurrency_primitives-journal-pending",
            tx: "01JB8Q2VA00000000000000009".parse().unwrap(),
            family: JournalFamily::Folder,
            phase: Phase::Prepared,
            repair_phase: None,
            direction: Direction::Back,
            protected: ResourceSet::new(),
        };
        let err = NoDelegate.recover(&lock, &[disclosed]).unwrap_err();
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
}
