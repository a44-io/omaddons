use std::path::PathBuf;

use super::super::Fixture;
use super::found_ids_with;
use crate::common::doctor::test_fixers::{ENV, REPAIRED, Side, WRITE, plant_write, test_dir};
use crate::common::home::Home;

/// `fm-test-write` (test-only, P3, about the machine): `<state>/doctor-test/a.write`
/// holding `broken\n`, which is not `repaired\n`. It exists only to exercise
/// `doctor fix`, and only debug builds run with `OMADDONS_TEST_FIXERS=1` register it.
///
/// The finding: `instance` `fm-test-write@machine:path=<the file>`, evidence `path` and
/// `kind: file`. Its fixer writes `repaired\n` (mode 0644), keeping the old bytes as the
/// run's backup; enabled, no `--force`. Healthy: the file holds `repaired\n`.
pub struct TestWrite;

impl Fixture for TestWrite {
    fn id(&self) -> &'static str {
        WRITE
    }

    fn corrupt(&self, home: &Home) {
        plant_write(home, "a.write", b"broken\n");
    }

    fn assert_healthy(&self, home: &Home) {
        let ids = found_ids_with(home, &[], &[ENV]);
        assert!(!ids.iter().any(|id| id == WRITE), "{ids:?}");
        let file = test_dir(home, Side::State).join("a.write");
        assert_eq!(std::fs::read(file).unwrap(), REPAIRED);
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
