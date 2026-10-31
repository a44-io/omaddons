use std::cell::RefCell;
use std::path::PathBuf;

use super::super::Fixture;
use super::{Tx, assert_cleared};
use crate::common::doctor::plant::{self, JournalEdit};
use crate::common::home::Home;

const ID: &str = "fm-concurrency_primitives-journal-step-unrecognized";
const TARGET: &str = "retail";

/// `fm-concurrency_primitives-journal-step-unrecognized` (P0, cached, on retail): an
/// interrupted change stopped at a step in neither its before nor its after state.
/// `corrupt` installs `gh:o/unrecognized` (not installed yet) on retail and kills it after
/// its step 1 was journaled and before that rename ran (`engine.swap.before_rename=abort`),
/// then raises that `step` line's `from_id.ino` by 1,000,000, so the staged folder is not
/// the one the step recorded (`plant::journal_step_unrecognized`). E11's verdict:
/// `StepUnrecognized { step: 1 }`; recovery stops for manual repair.
///
/// The finding: instance `…@<retail's key>:tx=<tx>`; evidence `reason:
/// step_unrecognized`, `step: 1`, `kind: install`. Remedy: write the journal back as the
/// crash left it, then a change that recovers (`config set check_interval_hours 6`).
/// Subject: the transaction's journal set.
#[derive(Default)]
pub struct JournalStepUnrecognized {
    tx: Tx,
    edit: RefCell<Option<JournalEdit>>,
}

impl Fixture for JournalStepUnrecognized {
    fn id(&self) -> &'static str {
        ID
    }

    fn corrupt(&self, home: &Home) {
        let (tx, edit) = plant::journal_step_unrecognized(home, TARGET);
        self.tx.set(tx);
        self.edit.replace(Some(edit));
    }

    fn assert_healthy(&self, home: &Home) {
        assert_cleared(home, ID, Some(&self.tx.get()), &self.run_env());
    }

    fn subject(&self, home: &Home) -> Vec<PathBuf> {
        plant::journal_set(home, &self.tx.get(), Some(TARGET))
    }

    fn remedy(&self, home: &Home) {
        if let Some(edit) = self.edit.take() {
            edit.undo();
        }
        plant::recover(home);
    }
}
