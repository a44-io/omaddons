//! Every fixture, one module each, named after its failure mode's id in snake case.

pub mod fm_concurrency_primitives_journal_marker_mismatch;
pub mod fm_concurrency_primitives_journal_pending;
pub mod fm_concurrency_primitives_journal_preservation_incomplete;
pub mod fm_concurrency_primitives_journal_step_unrecognized;
pub mod fm_concurrency_primitives_journal_too_new;
pub mod fm_concurrency_primitives_journal_unattributed;
pub mod fm_concurrency_primitives_journal_unexpected_state;
pub mod fm_concurrency_primitives_journal_unreadable;
pub mod fm_concurrency_primitives_journal_waits_for_game;
pub mod fm_concurrency_primitives_lock_held_long;
pub mod fm_state_files_check_incomplete;
pub mod fm_state_files_detector_crashed;
pub mod fm_test_flatten;
pub mod fm_test_quarantine;
pub mod fm_test_write;

use std::cell::RefCell;

use super::Fixture;
use crate::common::doctor::plant;
use crate::common::home::Home;

/// Every fixture of a catalog row, in id order.
pub fn all() -> Vec<Box<dyn Fixture>> {
    vec![
        Box::new(fm_concurrency_primitives_journal_marker_mismatch::JournalMarkerMismatch::default()),
        Box::new(fm_concurrency_primitives_journal_pending::JournalPending::default()),
        Box::new(
            fm_concurrency_primitives_journal_preservation_incomplete::JournalPreservationIncomplete::default(),
        ),
        Box::new(fm_concurrency_primitives_journal_step_unrecognized::JournalStepUnrecognized::default()),
        Box::new(fm_concurrency_primitives_journal_too_new::JournalTooNew::default()),
        Box::new(fm_concurrency_primitives_journal_unattributed::JournalUnattributed::default()),
        Box::new(fm_concurrency_primitives_journal_unexpected_state::JournalUnexpectedState::default()),
        Box::new(fm_concurrency_primitives_journal_unreadable::JournalUnreadable::default()),
        Box::new(fm_concurrency_primitives_journal_waits_for_game::JournalWaitsForGame::default()),
        Box::new(fm_concurrency_primitives_lock_held_long::LockHeldLong::default()),
        Box::new(fm_state_files_check_incomplete::CheckIncomplete),
        Box::new(fm_state_files_detector_crashed::DetectorCrashed),
    ]
}

/// The test-only fixers' fixtures, in id order: rows registered only in debug builds run
/// with `OMADDONS_TEST_FIXERS=1` (each fixture's `run_env`), so never among [`all`].
pub fn test_fixers() -> Vec<Box<dyn Fixture>> {
    vec![
        Box::new(fm_test_flatten::TestFlatten),
        Box::new(fm_test_quarantine::TestQuarantine),
        Box::new(fm_test_write::TestWrite),
    ]
}

/// The ids a plain `doctor --json` (plus `args`) of `home` reports.
pub fn found_ids(home: &Home, args: &[String]) -> Vec<String> {
    found_ids_with(home, args, &[])
}

/// [`found_ids`] with extra environment (`OMADDONS_TEST_FIXERS`).
pub fn found_ids_with(home: &Home, args: &[String], env: &[(&str, &str)]) -> Vec<String> {
    let mut argv: Vec<String> = vec!["doctor".to_owned(), "--json".to_owned()];
    argv.extend(args.iter().cloned());
    let run = home.exec_with(&argv, env);
    let envelope = crate::common::doctor::check_envelope(&run.stdout, &run.stderr, run.code);
    envelope["findings"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|f| f["id"].as_str().map(str::to_owned))
        .collect()
}

/// The transaction a journal fixture planted, once its `corrupt` ran.
#[derive(Default)]
pub struct Tx(RefCell<Option<String>>);

impl Tx {
    pub fn set(&self, tx: String) {
        self.0.replace(Some(tx));
    }

    /// The transaction; `corrupt` must have run.
    pub fn get(&self) -> String {
        self.0
            .borrow()
            .clone()
            .expect("corrupt plants the journal first")
    }
}

/// A journal or lock fixture's `assert_healthy`: `doctor --json --only <id>` (with the
/// fixture's `env`) finds no instance of `id`, and `<state>/journal/` holds no journal of
/// `tx`.
pub fn assert_cleared(home: &Home, id: &str, tx: Option<&str>, env: &[(String, String)]) {
    let env: Vec<(&str, &str)> = env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let ids = found_ids_with(home, &["--only".to_owned(), id.to_owned()], &env);
    assert!(
        !ids.iter().any(|found| found == id),
        "{id} still found: {ids:?}"
    );
    if let Some(tx) = tx {
        let left = plant::journals(home);
        assert!(
            !left.iter().any(|(t, _)| t == tx),
            "{tx}'s journal is still there: {left:?}"
        );
    }
}
