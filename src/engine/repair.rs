//! The repair transaction (E5, M4-D31): the doctor's fixes, made as safe and as undoable
//! as an install.
//!
//! 1. **Lower** ([`lower`]): every fix of a run becomes one immutable, ordered list of
//!    primitive steps (`write_file`, `chmod`, `rename_noreplace`), each with its endpoint
//!    predicates, the resources it writes, its reserved backup and its quarantine
//!    destination. Everything a repair may never do (a write below `WTF/`, a rename
//!    across mounts, a path outside the doctor's scopes) is refused there, before
//!    anything is written.
//! 2. **Journal**: `<state>/journal/<tx>.repair.jsonl` opens with a `prepared` line that
//!    holds every fix and step, and each primitive is written ahead as its own `step`
//!    line.
//! 3. **Commit**, under the global lock: each primitive through fsx, its predicates
//!    checked immediately before it; a failure rolls the steps that ran back by their
//!    inverses.
//! 4. **Recover**: every command that takes the lock settles a repair journal a crash
//!    left, classifying each step by its predicates, which is why a step's `pre` and
//!    `post` are decided by lowering and never guessed later.
//!
//! [`RepairTx`] holds the transaction's id and in-flight lock from planning to cleanup.

pub mod lower;

pub use lower::{LowerCtx, LowerError, Lowered, LoweredStep, PlannedFix, RefusedReason, lower};

use std::path::PathBuf;

use crate::error::Result;
use crate::fsx::InflightLock;
use crate::model::TxId;
use crate::state::paths::Paths;

/// One repair transaction: its id and its in-flight lock, `<state>/inflight/<tx>.lock`,
/// held from planning to cleanup so no sweep takes what it stages (§6.14.2 step 1).
/// Lowering needs the id: the client-side quarantine and backups are named for it.
#[derive(Debug)]
pub struct RepairTx {
    tx: TxId,
    inflight: InflightLock,
}

impl RepairTx {
    /// A fresh transaction id, and its in-flight lock created and held.
    pub fn begin(paths: &Paths) -> Result<Self> {
        let tx = TxId::new();
        let inflight = InflightLock::create(paths, &tx)?;
        Ok(Self { tx, inflight })
    }

    pub fn tx(&self) -> TxId {
        self.tx
    }

    /// The lock, for the stage and sweep calls that take one.
    pub fn inflight(&self) -> &InflightLock {
        &self.inflight
    }
}

/// A published doctor run directory, `<state>/doctor/runs/<id>` (E3's runtime makes it),
/// which receives a repair's home-side backups and quarantine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepairRun {
    pub dir: PathBuf,
    /// The directory's name, as `Resource::backup` names a backup input.
    pub id: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_repair_tx_holds_its_inflight_lock_until_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::with_roots(
            dir.path(),
            dir.path().join("c"),
            dir.path().join("s"),
            dir.path().join("k"),
        );
        let repair = RepairTx::begin(&paths).unwrap();
        let tx = repair.tx();
        assert_eq!(repair.inflight().tx(), tx);
        assert!(paths.inflight_lock(&tx).is_file());
        assert!(
            InflightLock::is_live(&paths, &tx).unwrap(),
            "held while planning"
        );
        drop(repair);
        assert!(
            !InflightLock::is_live(&paths, &tx).unwrap(),
            "free once dropped"
        );
    }
}
