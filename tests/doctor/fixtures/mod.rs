//! Every fixture, one module each, named after its failure mode's id in snake case.

pub mod fm_state_files_check_incomplete;
pub mod fm_state_files_detector_crashed;
pub mod fm_test_flatten;
pub mod fm_test_quarantine;
pub mod fm_test_write;

use super::Fixture;
use crate::common::home::Home;

/// Every fixture of a catalog row, in id order.
pub fn all() -> Vec<Box<dyn Fixture>> {
    vec![
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
