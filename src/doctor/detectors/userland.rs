//! `userland_state`: clients the registry remembers.

use crate::doctor::budget::Budget;
use crate::doctor::finding::{Confidence, Detected, Evidence, Observation, Subject};
use crate::doctor::selection::Selection;
use crate::doctor::view::DoctorView;

/// `target-vanished`: a registered client whose directory is gone.
pub(crate) fn target_vanished(view: &DoctorView, _: &Selection, _: &mut Budget) -> Detected {
    let found = view
        .status_facts()
        .targets
        .iter()
        .filter_map(|t| {
            let dir = t.vanished.as_ref()?;
            Some(Observation {
                subject: Subject::target(&t.id, t.key),
                title: "The client's directory is gone".to_owned(),
                confidence: Confidence::High,
                addon: None,
                folder: None,
                evidence: Evidence {
                    path: Some(dir.clone()),
                    ..Evidence::default()
                },
            })
        })
        .collect();
    Detected {
        found,
        incomplete: None,
    }
}
