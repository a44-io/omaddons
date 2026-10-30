//! `state_files`: records that disagree with the disk.

use crate::doctor::budget::Budget;
use crate::doctor::finding::{Confidence, Detected, Evidence, Observation, Subject};
use crate::doctor::selection::Selection;
use crate::doctor::view::DoctorView;
use crate::drift::DriftState;

/// `record-folder-missing`: one finding per managed addon whose folder is gone.
pub(crate) fn record_folder_missing(view: &DoctorView, _: &Selection, _: &mut Budget) -> Detected {
    let found = view
        .status_facts()
        .targets
        .iter()
        .flat_map(|t| {
            t.drift
                .iter()
                .filter(|d| d.state == DriftState::Missing)
                .map(|d| Observation {
                    subject: Subject::target(&t.id, t.key).addon(&d.addon),
                    title: format!("{}'s folder is gone", d.addon),
                    confidence: Confidence::High,
                    addon: Some(d.addon.clone()),
                    folder: None,
                    evidence: Evidence {
                        paths: (!d.missing_folders.is_empty()).then(|| d.missing_folders.clone()),
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
