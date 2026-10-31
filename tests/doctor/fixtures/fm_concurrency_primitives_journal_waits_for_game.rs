use std::cell::RefCell;
use std::path::PathBuf;

use super::super::Fixture;
use super::{Tx, assert_cleared};
use crate::common::doctor::plant;
use crate::common::home::Home;

const ID: &str = "fm-concurrency_primitives-journal-waits-for-game";
const TARGET: &str = "retail";

/// `fm-concurrency_primitives-journal-waits-for-game` (P2, cached, on retail): an
/// interrupted change to `WTF/` that waits for the game to exit. `corrupt` gives retail a
/// `WTF/` (fake names only) and a manual backup of it, kills a whole `backup restore` of
/// that backup right after its exchange (`engine.restore.exchanged=abort`: a `writes_wtf`
/// journal with step 1 done), and builds a fake `/proc` in which retail's `Wow.exe` runs
/// (`plant::journal_waits_for_game`). Every diagnosing run gets that fake as
/// `OMADDONS_PROC_ROOT` (`run_env`). E11's verdict: `WaitsForGame`, since recovery never
/// moves `WTF/` while its game runs.
///
/// The finding: instance `…@<retail's key>:tx=<tx>`; evidence `reason:
/// saved_variables`, `kind: restore`, `family: folder`. Remedy: the game exits, so a
/// change that runs discovery recovers without the fake process (an install from a local
/// archive), rolling the restore back. Subject: the transaction's journal set (its
/// `.wtf` journal, stage and snapshots).
#[derive(Default)]
pub struct JournalWaitsForGame {
    tx: Tx,
    proc: RefCell<Option<PathBuf>>,
}

impl Fixture for JournalWaitsForGame {
    fn id(&self) -> &'static str {
        ID
    }

    fn corrupt(&self, home: &Home) {
        let (tx, proc) = plant::journal_waits_for_game(home, TARGET);
        self.tx.set(tx);
        self.proc.replace(Some(proc.root().to_path_buf()));
    }

    fn assert_healthy(&self, home: &Home) {
        assert_cleared(home, ID, Some(&self.tx.get()), &self.run_env());
    }

    fn subject(&self, home: &Home) -> Vec<PathBuf> {
        plant::journal_set(home, &self.tx.get(), Some(TARGET))
    }

    fn run_env(&self) -> Vec<(String, String)> {
        self.proc
            .borrow()
            .iter()
            .map(|root| ("OMADDONS_PROC_ROOT".to_owned(), root.display().to_string()))
            .collect()
    }

    fn remedy(&self, home: &Home) {
        plant::recover_wtf(home, TARGET);
    }
}
