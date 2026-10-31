use std::path::PathBuf;

use super::super::Fixture;
use super::found_ids_with;
use crate::common::doctor::test_fixers::{ENV, Kind, QUARANTINE, Side, plant_quarantine, test_dir};
use crate::common::home::Home;

/// `fm-test-quarantine` (test-only, P3, about the machine): a file
/// `<state>/doctor-test/b.quarantine`. It exists only to exercise `doctor fix`, and only
/// debug builds run with `OMADDONS_TEST_FIXERS=1` register it.
///
/// The finding: evidence `path` and `kind`. Its fixer sets the entry aside into its
/// side's quarantine root (here `<run>/quarantine/state/doctor-test/b.quarantine`),
/// deleting nothing. It needs `--force` and is disabled, so it runs only with
/// `--only fm-test-quarantine --force --yes`. Healthy: nothing is left at the path.
pub struct TestQuarantine;

impl Fixture for TestQuarantine {
    fn id(&self) -> &'static str {
        QUARANTINE
    }

    fn corrupt(&self, home: &Home) {
        plant_quarantine(home, Side::State, "b.quarantine", Kind::File);
    }

    fn assert_healthy(&self, home: &Home) {
        let ids = found_ids_with(home, &[], &[ENV]);
        assert!(!ids.iter().any(|id| id == QUARANTINE), "{ids:?}");
        let path = test_dir(home, Side::State).join("b.quarantine");
        assert!(std::fs::symlink_metadata(path).is_err());
    }

    fn subject(&self, home: &Home) -> Vec<PathBuf> {
        vec![test_dir(home, Side::State)]
    }

    fn fixer(&self) -> bool {
        true
    }

    fn run_env(&self) -> Vec<(String, String)> {
        vec![(ENV.0.to_owned(), ENV.1.to_owned())]
    }
}
