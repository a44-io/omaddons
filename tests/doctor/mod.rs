//! The doctor's fixtures (M4-D47): one per failure mode, each breaking a home on purpose.
//! A test crate includes this directory as `#[path = "doctor/mod.rs"] mod fixture;`
//! (beside its own `mod common;`, which the fixtures build on).

use std::path::PathBuf;

use crate::common::home::Home;

/// One failure mode, broken on purpose (M4-D47). The doc comment of each implementation
/// is its README: what it breaks, and the finding a diagnose then reports.
pub trait Fixture {
    /// The failure mode's id.
    fn id(&self) -> &'static str;
    /// Breaks `home` so a diagnose finds this failure mode.
    fn corrupt(&self, home: &Home);
    /// Asserts a diagnose of `home` finds no instance of this failure mode.
    fn assert_healthy(&self, home: &Home);
    /// The paths a fixer of this failure mode would change, defined before the fixer
    /// lands when one is planned; empty when none would change anything.
    fn subject(&self, home: &Home) -> Vec<PathBuf>;
    /// A fixer repairs it.
    fn fixer(&self) -> bool {
        false
    }
    /// The manual remedy this finding's remediation advises, applied by hand (quit the
    /// game, finish the journal with a build that can read it, release the lock), after
    /// which `assert_healthy` holds. A fixer's fixture leaves it empty.
    fn remedy(&self, _home: &Home) {}
    /// Environment the diagnosing run needs (a failpoint, for a runtime row).
    fn run_env(&self) -> Vec<(String, String)> {
        Vec::new()
    }
    /// Arguments the diagnosing run needs (`--only`, `--budget`, for a runtime row).
    fn run_args(&self) -> Vec<String> {
        Vec::new()
    }
}

pub mod fixtures;
