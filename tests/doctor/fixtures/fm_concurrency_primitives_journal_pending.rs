use std::path::PathBuf;

use super::super::Fixture;
use super::{Tx, assert_cleared};
use crate::common::doctor::plant;
use crate::common::home::Home;

const ID: &str = "fm-concurrency_primitives-journal-pending";
const TARGET: &str = "retail";

/// `fm-concurrency_primitives-journal-pending` (P0, cached, on retail): an interrupted
/// change that recovery can settle. `corrupt` installs `gh:o/stuck` on retail and kills
/// it right after its journal's `prepared` line (`engine.journal.prepared=abort`), before
/// any step ran (`plant::journal_pending`). E11's verdict: `Recoverable { Prepared, Back
/// }`, so the next change rolls it back.
///
/// The finding: instance `…@<retail's key>:tx=<tx>`; evidence `tx`, `family: folder`,
/// `kind: install`, `phase: prepared`, and no `reason`. Fixer: the delegation (F2,
/// M4-D37), `doctor fix --only fm-concurrency_primitives-journal-pending -t retail`, which
/// runs omaddons' own recovery for exactly this transaction. Its inverse is M4-D37's, not
/// `doctor undo`: nothing after a rollback (this one), `omaddons undo <tx>` after a
/// roll-forward. Remedy by hand: any change that recovers (`config set
/// check_interval_hours 6`). Subject: the transaction's journal set.
#[derive(Default)]
pub struct JournalPending {
    tx: Tx,
}

impl Fixture for JournalPending {
    fn id(&self) -> &'static str {
        ID
    }

    fn corrupt(&self, home: &Home) {
        self.tx.set(plant::journal_pending_tx(home, TARGET));
    }

    fn assert_healthy(&self, home: &Home) {
        assert_cleared(home, ID, Some(&self.tx.get()), &self.run_env());
    }

    fn subject(&self, home: &Home) -> Vec<PathBuf> {
        plant::journal_set(home, &self.tx.get(), Some(TARGET))
    }

    fn fixer(&self) -> bool {
        true
    }

    fn remedy(&self, home: &Home) {
        plant::recover(home);
    }
}
