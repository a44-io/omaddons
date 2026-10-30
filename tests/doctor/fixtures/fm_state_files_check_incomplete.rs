use std::path::PathBuf;

use omaddons::doctor::registry::{FAILURE_MODES, FailureMode, Tier};

use super::super::Fixture;
use super::found_ids;
use crate::common::home::Home;

const ID: &str = "fm-state_files-check-incomplete";

/// `fm-state_files-check-incomplete`: a check its deadline stops. No file on disk triggers
/// it, so the home stays as it is and the diagnosing run selects only the cheapest cached
/// check (`--only <id> --budget <its cost>ms`) while `doctor.detector.panic=sleep:50` spends
/// that budget before the check starts: never started is incomplete, never a pass.
///
/// The finding: P3, about the machine, evidence `{detector: <the check>, reason:
/// "deadline", deadline_ms: <the budget>}` and no `max_entries`. The run exits 1, never
/// `DONE_OK`. No fixer: the remediation reruns the check with ten times the budget.
pub struct CheckIncomplete;

impl CheckIncomplete {
    /// The cached row with the smallest declared cost, the first in registry order.
    pub fn row() -> &'static FailureMode {
        FAILURE_MODES
            .iter()
            .filter(|row| row.tier == Tier::Cached && row.detect.is_some())
            .min_by_key(|row| row.cost_ms)
            .unwrap()
    }

    /// The budget the run is given, in milliseconds: the row's cost, at least 1.
    pub fn budget_ms() -> u32 {
        Self::row().cost_ms.max(1)
    }
}

impl Fixture for CheckIncomplete {
    fn id(&self) -> &'static str {
        ID
    }

    fn corrupt(&self, _: &Home) {}

    fn assert_healthy(&self, home: &Home) {
        let ids = found_ids(home, &self.run_args());
        assert!(!ids.iter().any(|id| id == ID), "{ids:?}");
    }

    fn subject(&self, _: &Home) -> Vec<PathBuf> {
        Vec::new()
    }

    fn run_env(&self) -> Vec<(String, String)> {
        vec![(
            "OMADDONS_FAILPOINTS".to_owned(),
            "doctor.detector.panic=sleep:50".to_owned(),
        )]
    }

    fn run_args(&self) -> Vec<String> {
        vec![
            "--only".to_owned(),
            Self::row().id.to_owned(),
            "--budget".to_owned(),
            format!("{}ms", Self::budget_ms()),
        ]
    }
}
