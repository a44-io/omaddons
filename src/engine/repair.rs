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

pub mod commit;
pub mod inverse;
pub mod journal;
pub mod lower;
pub mod recover;

pub use commit::{CommitCtx, RepairCleanup, RepairMeta, RepairOutcome, abandon, commit};
pub use journal::{
    ClientDirEntry, FORMAT, ParsedRepair, PreparedRead, RepairLine, RepairPhase, RepairPrepared,
    ResourcesWire, RollbackTarget, history_line, read_prepared,
};
pub use lower::{LowerCtx, LowerError, Lowered, LoweredStep, PlannedFix, RefusedReason, lower};
pub use recover::{PendingRepair, pending_repairs};

use std::path::{Path, PathBuf};

use crate::error::Result;
use crate::fsx::{InflightLock, canonical_lenient};
use crate::model::TxId;
use crate::state::paths::{Paths, REPAIR_SUFFIX};

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

// ---------------------------------------------------------------------------------------
// Journal names (M4-D31: this module is the only parser of repair names).

/// What a journal restored by a repair's undo is named until its activation renames it to
/// its own name (M4-D38): `<tx>.reactivating.jsonl`, which neither `paths::tx_named` nor
/// the folder listing parses, so ordinary recovery, the orphan sweep and older builds
/// never take it for a journal of theirs.
pub const REACTIVATING_SUFFIX: &str = ".reactivating";

/// Which journal a file of `<state>/journal/` is, by its name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum JournalName {
    /// `<tx>.jsonl`: a folder transaction.
    Folder,
    /// `<tx>.wtf.jsonl`: a transaction that puts settings back.
    Wtf,
    /// `<tx>.repair.jsonl`: a doctor repair.
    Repair,
    /// `<tx>.reactivating.jsonl`: a journal under its provisional name
    /// ([`REACTIVATING_SUFFIX`]).
    Reactivating,
}

/// The transaction and the kind of journal `path` is: a file directly in
/// `<state>/journal/` (as `paths` names it, or canonical) named `<tx>.jsonl`,
/// `<tx>.wtf.jsonl`, `<tx>.repair.jsonl` or `<tx>.reactivating.jsonl`, the id written
/// exactly as a transaction id is. Anything else is `None`.
pub fn journal_file(paths: &Paths, path: &Path) -> Option<(TxId, JournalName)> {
    let parent = path.parent()?;
    let dir = paths.journal_dir();
    // ubs:ignore — comparing paths, not secrets
    if parent != dir && canonical_lenient(&dir).as_deref() != Some(parent) {
        return None;
    }
    let stem = path.file_name()?.to_str()?.strip_suffix(".jsonl")?;
    let (id, name) = [
        (".wtf", JournalName::Wtf),
        (REPAIR_SUFFIX, JournalName::Repair),
        (REACTIVATING_SUFFIX, JournalName::Reactivating),
    ]
    .into_iter()
    .find_map(|(suffix, name)| stem.strip_suffix(suffix).map(|id| (id, name)))
    .unwrap_or((stem, JournalName::Folder));
    let tx = id.parse::<TxId>().ok().filter(|tx| tx.to_string() == id)?;
    Some((tx, name))
}

/// `<state>/journal/<tx>.reactivating.jsonl`: where a repair's undo restores the journal
/// of `tx` before its activation (E14's third part gives it its meaning).
pub fn provisional_journal(paths: &Paths, tx: &TxId) -> PathBuf {
    paths
        .journal_dir()
        .join(format!("{tx}{REACTIVATING_SUFFIX}.jsonl"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn journal_file_parses_the_four_names_and_nothing_else() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::with_roots(
            dir.path(),
            dir.path().join("c"),
            dir.path().join("s"),
            dir.path().join("k"),
        );
        let tx = TxId::new();
        let journal = paths.journal_dir();
        let at = |name: String| journal.join(name);
        /// A case: its name, the path, and what it parses to.
        type Case = (&'static str, PathBuf, Option<(TxId, JournalName)>);
        let cases: Vec<Case> = vec![
            (
                "folder",
                at(format!("{tx}.jsonl")),
                Some((tx, JournalName::Folder)),
            ),
            (
                "wtf",
                at(format!("{tx}.wtf.jsonl")),
                Some((tx, JournalName::Wtf)),
            ),
            (
                "repair",
                at(format!("{tx}.repair.jsonl")),
                Some((tx, JournalName::Repair)),
            ),
            (
                "reactivating",
                provisional_journal(&paths, &tx),
                Some((tx, JournalName::Reactivating)),
            ),
            (
                "wtf_reactivating",
                at(format!("{tx}.wtf.reactivating.jsonl")),
                None,
            ),
            ("repair_wtf", at(format!("{tx}.repair.wtf.jsonl")), None),
            ("no_extension", at(tx.to_string()), None),
            ("lock", at(format!("{tx}.lock")), None),
            (
                "lowercase_id",
                at(format!("{}.jsonl", tx.to_string().to_lowercase())),
                None,
            ),
            ("not_an_id", at("01NOTAULID.jsonl".to_owned()), None),
            (
                "outside_journal",
                paths.inflight_dir().join(format!("{tx}.jsonl")),
                None,
            ),
            (
                "below_journal",
                journal.join("sub").join(format!("{tx}.jsonl")),
                None,
            ),
        ];
        for (case, path, want) in cases {
            assert_eq!(
                journal_file(&paths, &path),
                want,
                "{case}: {}",
                path.display()
            );
        }
    }

    #[test]
    fn provisional_is_invisible_to_tx_named() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::with_roots(
            dir.path(),
            dir.path().join("c"),
            dir.path().join("s"),
            dir.path().join("k"),
        );
        let tx = TxId::new();
        let provisional = provisional_journal(&paths, &tx);
        let name = provisional.file_name().unwrap().to_str().unwrap();
        let stem = name.strip_suffix(".jsonl").unwrap();
        assert_eq!(crate::state::paths::tx_named(stem), None, "{name}");
        assert_eq!(
            provisional.parent(),
            Some(paths.journal_dir().as_path()),
            "in the journal directory"
        );
    }

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
