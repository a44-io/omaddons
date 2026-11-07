//! `state_files`: records that no longer match the disk.

use super::TargetFacts;
use crate::doctor::budget::Budget;
use crate::doctor::finding::{Confidence, Detected, Evidence, Observation, Subject};
use crate::doctor::selection::Selection;
use crate::doctor::view::DoctorView;
use crate::drift::{DriftMode, DriftState};

/// `record-folder-missing`, from the Discovery and Records layers: a presence check, as
/// `status --cached` makes it (a deeper report answers: the gone folders are the same).
pub(crate) fn record_folder_missing(
    view: &DoctorView,
    sel: &Selection,
    _: &mut Budget,
) -> Detected {
    let targets = super::drift_facts(view, sel, DriftMode::Presence);
    Detected::complete(record_folder_missing_in(&targets))
}

/// `record-folder-missing`'s rule: one finding per managed addon whose folder is gone.
pub(crate) fn record_folder_missing_in(targets: &[TargetFacts]) -> Vec<Observation> {
    targets
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
        .collect()
}
