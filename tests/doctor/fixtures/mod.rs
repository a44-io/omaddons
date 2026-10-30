//! Every fixture, one module each, named after its failure mode's id in snake case.

pub mod fm_state_files_check_incomplete;
pub mod fm_state_files_detector_crashed;

use super::Fixture;
use crate::common::home::Home;

/// Every fixture, in id order.
pub fn all() -> Vec<Box<dyn Fixture>> {
    vec![
        Box::new(fm_state_files_check_incomplete::CheckIncomplete),
        Box::new(fm_state_files_detector_crashed::DetectorCrashed),
    ]
}

/// The ids a plain `doctor --json` (plus `args`) of `home` reports.
pub fn found_ids(home: &Home, args: &[String]) -> Vec<String> {
    let mut argv: Vec<String> = vec!["doctor".to_owned(), "--json".to_owned()];
    argv.extend(args.iter().cloned());
    let run = home.exec_with(&argv, &[]);
    let envelope = crate::common::doctor::check_envelope(&run.stdout, &run.stderr, run.code);
    envelope["findings"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|f| f["id"].as_str().map(str::to_owned))
        .collect()
}
