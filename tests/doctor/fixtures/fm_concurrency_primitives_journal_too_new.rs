use std::cell::RefCell;
use std::path::PathBuf;

use super::super::Fixture;
use super::{Tx, assert_cleared};
use crate::common::doctor::plant::{self, JournalEdit};
use crate::common::home::Home;

const ID: &str = "fm-concurrency_primitives-journal-too-new";
const TARGET: &str = "retail";

/// `fm-concurrency_primitives-journal-too-new` (P0, cached, on retail): an interrupted
/// change started by a newer omaddons, whose journal this build does not read.
/// `corrupt` plants `journal-pending`'s crashed install, then sets its `prepared` line's
/// top-level `schema_version` to `"9.0"`, a newer major (`plant::journal_too_new`).
/// E11's verdict: `TooNew`; recovery leaves it whole, and refuses every change on retail.
///
/// The finding: instance `…@<retail's key>:tx=<tx>`; evidence `reason:
/// journal_too_new`, `format: "9.0"`, and no `phase` (its lines are not trusted).
/// Remedy: finish it with a build that reads it, here by writing the journal back as the
/// crash left it, then a change that recovers (`config set check_interval_hours 6`).
/// Subject: the transaction's journal set.
#[derive(Default)]
pub struct JournalTooNew {
    tx: Tx,
    edit: RefCell<Option<JournalEdit>>,
}

impl Fixture for JournalTooNew {
    fn id(&self) -> &'static str {
        ID
    }

    fn corrupt(&self, home: &Home) {
        let (tx, edit) = plant::journal_too_new(home, TARGET);
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
