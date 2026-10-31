use std::cell::RefCell;
use std::path::PathBuf;

use super::super::Fixture;
use super::{Tx, assert_cleared};
use crate::common::doctor::plant::{self, Moved};
use crate::common::home::Home;

const ID: &str = "fm-concurrency_primitives-journal-unexpected-state";
const TARGET: &str = "retail";

/// `fm-concurrency_primitives-journal-unexpected-state` (P0, cached, on retail): an
/// interrupted change to `WTF/` stopped at a step it cannot place. `corrupt` kills a
/// whole `backup restore` on retail right after its exchange, as `journal-waits-for-game`
/// does but with no game running; then it replaces both sides of that step 1: the staged
/// and the live `WTF/` are moved into `<root>/aside/`, and an empty `WTF/` is made at the
/// live path (`plant::journal_unexpected_state`). E11's verdict: `UnexpectedState {
/// step: 1 }`; recovery stops and leaves everything as found.
///
/// The finding: instance `…@<retail's key>:tx=<tx>`; evidence `reason:
/// unexpected_state`, `step: 1`, `kind: restore`. Remedy: remove the empty `WTF/` and move
/// both trees back, then a change that runs discovery recovers (an install from a local
/// archive). Subject: the transaction's journal set.
#[derive(Default)]
pub struct JournalUnexpectedState {
    tx: Tx,
    moved: RefCell<Option<Moved>>,
}

impl Fixture for JournalUnexpectedState {
    fn id(&self) -> &'static str {
        ID
    }

    fn corrupt(&self, home: &Home) {
        let (tx, moved) = plant::journal_unexpected_state(home, TARGET);
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
    }
}
