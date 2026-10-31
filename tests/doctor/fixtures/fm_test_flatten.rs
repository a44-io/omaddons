use std::path::PathBuf;

use super::super::Fixture;
use super::found_ids_with;
use crate::common::doctor::test_fixers::{ENV, FLATTEN, plant_flatten, register};
use crate::common::home::Home;

/// `fm-test-flatten` (test-only, P3, on retail): an unmanaged
/// `Interface/AddOns/TestWrapA/` whose only entry is the addon folder `Inner/`, on a
/// registered retail client. It exists only to exercise `doctor fix`, and only debug
/// builds run with `OMADDONS_TEST_FIXERS=1` register it.
///
/// The finding: evidence `path` (the wrapper) and `paths` (the inner folder). Its fixer
/// moves `Inner/` up into `AddOns/` and sets the empty wrapper aside in the repair's
/// client-side quarantine (`<meta>/snapshots/<tx>.repair/quarantine/`). It is disabled
/// (runs only with `--only fm-test-flatten`) and refused while the client runs.
/// Healthy: `AddOns/Inner/` and no wrapper.
pub struct TestFlatten;

impl Fixture for TestFlatten {
    fn id(&self) -> &'static str {
        FLATTEN
    }

    fn corrupt(&self, home: &Home) {
        register(home, "retail");
        plant_flatten(home, "_retail_", "TestWrapA", "Inner");
    }

    fn assert_healthy(&self, home: &Home) {
        let ids = found_ids_with(home, &[], &[ENV]);
        assert!(!ids.iter().any(|id| id == FLATTEN), "{ids:?}");
        let addons = home.addons("_retail_");
        assert!(addons.join("Inner").is_dir());
        assert!(!addons.join("TestWrapA").exists());
    }

    fn subject(&self, home: &Home) -> Vec<PathBuf> {
        vec![home.addons("_retail_")]
    }

    fn fixer(&self) -> bool {
        true
    }

    fn run_env(&self) -> Vec<(String, String)> {
        vec![(ENV.0.to_owned(), ENV.1.to_owned())]
    }
}
