//! Which failure modes a run checks. E2 adds `-t`, `--skip`, subsystem names and the
//! validation of `--only` (the closest id on a miss).

use std::collections::BTreeSet;

use super::registry::FailureMode;

/// Every failure mode, or only the ones named.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Selection {
    only: Option<BTreeSet<&'static str>>,
}

impl Selection {
    pub fn all() -> Self {
        Self { only: None }
    }

    pub fn only(ids: &[&'static str]) -> Self {
        Self {
            only: Some(ids.iter().copied().collect()),
        }
    }

    pub fn includes(&self, row: &FailureMode) -> bool {
        self.only.as_ref().is_none_or(|only| only.contains(row.id))
    }
}
