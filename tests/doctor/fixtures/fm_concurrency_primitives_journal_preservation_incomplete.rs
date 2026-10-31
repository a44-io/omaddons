use std::cell::RefCell;
use std::path::PathBuf;

use super::super::Fixture;
use super::{Tx, assert_cleared};
use crate::common::doctor::plant::{self, Moved};
use crate::common::home::Home;

const ID: &str = "fm-concurrency_primitives-journal-preservation-incomplete";
const TARGET: &str = "retail";

/// `fm-concurrency_primitives-journal-preservation-incomplete` (P1, cached, on retail):
/// an interrupted restore has not finished keeping the settings it displaced. `corrupt`
/// kills a whole `backup restore` on retail right after its exchange (no game running),
/// rewrites one SavedVariables file of the restored `WTF/` (a play session, so rolling
/// back must keep that tree as a `recovered` backup), then kills the recovering install
/// once that preservation intent is journaled and before it is done
/// (`engine.preserve.intent=abort`; `plant::journal_preservation_incomplete`). E11's
/// verdict: `PreservationIncomplete { stops: false }`; the next recovery resumes it with
/// the same backup id.
///
/// The finding: instance `…@<retail's key>:tx=<tx>`; evidence `reason:
/// preservation_incomplete`, `resumable: true`, `count` the trees to keep (at least 1),
/// `phase: preserve`. Remedy: a change that runs discovery recovers again (an install
/// from a local archive), which completes the preservation: a `recovered` backup then
/// exists. Subject: the transaction's journal set.
#[derive(Default)]
pub struct JournalPreservationIncomplete {
    tx: Tx,
}

impl Fixture for JournalPreservationIncomplete {
    fn id(&self) -> &'static str {
        ID
    }

    fn corrupt(&self, home: &Home) {
        self.tx
            .set(plant::journal_preservation_incomplete(home, TARGET));
    }

    fn assert_healthy(&self, home: &Home) {
        assert_cleared(home, ID, Some(&self.tx.get()), &self.run_env());
    }

    fn subject(&self, home: &Home) -> Vec<PathBuf> {
        plant::journal_set(home, &self.tx.get(), Some(TARGET))
    }

    fn remedy(&self, home: &Home) {
        plant::recover_wtf(home, TARGET);
        let recovered = plant::recovered_backups(home, TARGET);
        assert_eq!(recovered.len(), 1, "one recovered backup: {recovered:?}");
    }
}

/// The same row's stopping state, a variant beside [`JournalPreservationIncomplete`]
/// (`fixtures::variants()`): its `corrupt` plants that state, then moves the displaced
/// tree its pending intent names into `<root>/aside/` (`plant::journal_preservation_stops`),
/// so recovery can place it neither where it found it nor in the assembly. E11's verdict:
/// `PreservationIncomplete { stops: true }`; recovery stops with `state_corrupt`, keeping
/// everything.
///
/// The finding: the same instance shape; evidence `reason: preservation_incomplete`,
/// `resumable: false`, `count` at least 1, and the title "An interrupted change to WTF/
/// could not finish keeping the settings it displaced". Remedy: move the tree back, then
/// a change that runs discovery recovers and completes the preservation.
#[derive(Default)]
pub struct JournalPreservationStops {
    tx: Tx,
    moved: RefCell<Option<Moved>>,
}

impl Fixture for JournalPreservationStops {
    fn id(&self) -> &'static str {
        ID
    }

    fn corrupt(&self, home: &Home) {
        let (tx, moved) = plant::journal_preservation_stops(home, TARGET);
        self.tx.set(tx);
        self.moved.replace(Some(moved));
    }

    fn assert_healthy(&self, home: &Home) {
        assert_cleared(home, ID, Some(&self.tx.get()), &self.run_env());
    }

    fn subject(&self, home: &Home) -> Vec<PathBuf> {
        plant::journal_set(home, &self.tx.get(), Some(TARGET))
    }

    fn remedy(&self, home: &Home) {
        if let Some(moved) = self.moved.take() {
            moved.undo();
        }
        plant::recover_wtf(home, TARGET);
        let recovered = plant::recovered_backups(home, TARGET);
        assert_eq!(recovered.len(), 1, "one recovered backup: {recovered:?}");
    }
}
