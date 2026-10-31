use std::path::PathBuf;

use super::super::Fixture;
use super::{Tx, assert_cleared};
use crate::common::doctor::plant;
use crate::common::home::Home;

const ID: &str = "fm-concurrency_primitives-journal-unattributed";

/// `fm-concurrency_primitives-journal-unattributed` (P0, cached, about the machine): an
/// interrupted change no client can carry. `corrupt` writes `<state>/journal/<a fresh
/// id>.jsonl` holding the one complete line `{"type":"prepared","schema_version":"1.1"}`,
/// which names no target and does not decode (`plant::journal_unattributed`). E11's
/// verdict: `Unattributed { cause: Unreadable }`; recovery cannot say whose it is, so
/// every change on every client is refused.
///
/// The finding: instance `…@machine:tx=<tx>`; evidence `cause: unreadable`, `reason:
/// unattributed`, and no `target`. Remedy: move the file out of the journal directory
/// (into `<root>/aside/`). Subject: the journal file alone.
#[derive(Default)]
pub struct JournalUnattributed {
    tx: Tx,
}

impl Fixture for JournalUnattributed {
    fn id(&self) -> &'static str {
        ID
    }

    fn corrupt(&self, home: &Home) {
        self.tx.set(plant::journal_unattributed(home));
    }

    fn assert_healthy(&self, home: &Home) {
        assert_cleared(home, ID, Some(&self.tx.get()), &self.run_env());
    }

    fn subject(&self, home: &Home) -> Vec<PathBuf> {
        plant::journal_set(home, &self.tx.get(), None)
    }

    fn remedy(&self, home: &Home) {
        plant::set_journal_aside(home, &self.tx.get());
    }
}
