use std::path::PathBuf;

use super::super::Fixture;
use super::found_ids;
use crate::common::home::Home;

const ID: &str = "fm-state_files-detector-crashed";

/// `fm-state_files-detector-crashed`: a check that panics. No file on disk triggers it, so
/// the home stays as it is (its clients, nothing installed) and the diagnosing run arms
/// `doctor.detector.panic=panic@1`: the first check it runs panics.
///
/// The finding: P1, about the machine, `instance` `fm-state_files-detector-crashed@machine:
/// detector=<the check>`, evidence `detector` (the check's id) and `location` (where in
/// omaddons' source it panicked). The run goes on and exits 1. No fixer: a crash is a bug.
pub struct DetectorCrashed;

impl Fixture for DetectorCrashed {
    fn id(&self) -> &'static str {
        ID
    }

    fn corrupt(&self, _: &Home) {}

    fn assert_healthy(&self, home: &Home) {
        let ids = found_ids(home, &[]);
        assert!(!ids.iter().any(|id| id == ID), "{ids:?}");
    }

    fn subject(&self, _: &Home) -> Vec<PathBuf> {
        Vec::new()
    }

    fn run_env(&self) -> Vec<(String, String)> {
        vec![(
            "OMADDONS_FAILPOINTS".to_owned(),
            "doctor.detector.panic=panic@1".to_owned(),
        )]
    }
}
