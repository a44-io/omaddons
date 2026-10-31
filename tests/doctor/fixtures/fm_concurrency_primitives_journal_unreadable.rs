use std::cell::RefCell;
use std::path::PathBuf;

use super::super::Fixture;
use super::{Tx, assert_cleared};
use crate::common::doctor::plant::{self, JournalEdit};
use crate::common::home::Home;

const ID: &str = "fm-concurrency_primitives-journal-unreadable";
const TARGET: &str = "retail";

/// `fm-concurrency_primitives-journal-unreadable` (P0, cached, on retail): an
/// interrupted change whose journal holds a line this build cannot read. `corrupt` plants
/// `journal-pending`'s crashed install, then appends the complete line
/// `{"type":"from_a_later_build"}` (`plant::journal_unreadable`). E11's verdict:
/// `Unreadable`; recovery leaves it whole, and refuses every change on retail.
///
/// The finding: instance `…@<retail's key>:tx=<tx>`; evidence `reason:
/// journal_unreadable`, and no `phase`. Remedy: remove the line (the journal written back
/// as the crash left it), then a change that recovers (`config set
/// check_interval_hours 6`). Subject: the transaction's journal set.
#[derive(Default)]
pub struct JournalUnreadable {
    tx: Tx,
    edit: RefCell<Option<JournalEdit>>,
}

impl Fixture for JournalUnreadable {
    fn id(&self) -> &'static str {
        ID
    }

    fn corrupt(&self, home: &Home) {
        let (tx, edit) = plant::journal_unreadable(home, TARGET);
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
