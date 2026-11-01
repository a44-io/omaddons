use std::cell::RefCell;
use std::path::PathBuf;

use super::super::Fixture;
use super::{Tx, assert_cleared};
use crate::common::doctor::plant::{self, JournalEdit};
use crate::common::home::Home;

const ID: &str = "fm-concurrency_primitives-journal-marker-mismatch";
const TARGET: &str = "retail";
/// The client whose directory the journal is pointed at.
pub const OTHER: &str = "classic";

/// `fm-concurrency_primitives-journal-marker-mismatch` (P0, cached, on retail): an
/// interrupted change names a client directory that is no longer its client's.
/// `corrupt` registers classic (an addon installed there writes its marker), plants
/// `journal-pending`'s crashed install on retail, and sets that journal's `prepared`
/// line's `client_dir` to classic's client dir (`plant::journal_marker_mismatch`). E11's
/// verdict: `MarkerMismatch`; recovery never writes into a client its marker does not
/// name, and refuses every change on retail.
///
/// The finding: instance `…@<retail's key>:tx=<tx>`; evidence `reason: marker_mismatch`,
/// `path` classic's client dir, `expected` retail's key, `observed` classic's key.
/// Remedy: write the journal back as the crash left it, then a change that recovers
/// (`config set check_interval_hours 6`). Subject: the transaction's journal set, on
/// retail.
#[derive(Default)]
pub struct JournalMarkerMismatch {
    tx: Tx,
    edit: RefCell<Option<JournalEdit>>,
}

impl Fixture for JournalMarkerMismatch {
    fn id(&self) -> &'static str {
        ID
    }

    fn corrupt(&self, home: &Home) {
        let (tx, edit) = plant::journal_marker_mismatch(home, TARGET, OTHER);
        self.tx.set(tx);
        self.edit.replace(Some(edit));
    }

    fn assert_healthy(&self, home: &Home) {
        assert_cleared(home, ID, Some(&self.tx.get()), &self.run_env());
    }

    fn subject(&self, home: &Home) -> Vec<PathBuf> {
        plant::journal_set(home, &self.tx.get(), Some(TARGET))
    }

    /// The forced set-aside (F2); its inverse is `doctor undo`, while its guards hold.
    fn fixer(&self) -> bool {
        true
    }

    fn remedy(&self, home: &Home) {
        if let Some(edit) = self.edit.take() {
            edit.undo();
        }
        plant::recover(home);
    }
}
