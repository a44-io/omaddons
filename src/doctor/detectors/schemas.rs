//! `schemas`: state this build cannot write.

use crate::doctor::budget::Budget;
use crate::doctor::finding::{Confidence, Detected, Evidence, Observation, Subject};
use crate::doctor::selection::Selection;
use crate::doctor::view::DoctorView;

/// `state-version-newer`: a target's record written by a newer omaddons.
pub(crate) fn state_version_newer(view: &DoctorView, _: &Selection, _: &mut Budget) -> Detected {
    let found = view
        .status_facts()
        .targets
        .iter()
        .filter_map(|t| {
            let newer = t.newer_record.as_ref()?;
            Some(Observation {
                subject: Subject::target(&t.id, t.key).path(&newer.path),
                title: "State was written by a newer omaddons".to_owned(),
                confidence: Confidence::High,
                addon: None,
                folder: None,
                evidence: Evidence {
                    path: Some(newer.path.clone()),
                    observed: Some(newer.on_disk.to_string()),
                    expected: Some(newer.writes.to_string()),
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
