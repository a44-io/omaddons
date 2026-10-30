//! `schemas`: state this build cannot read or write.

use crate::doctor::budget::Budget;
use crate::doctor::finding::{Confidence, Detected, Evidence, Observation, Subject};
use crate::doctor::registry::ids;
use crate::doctor::selection::Selection;
use crate::doctor::view::DoctorView;

/// `state-corrupt`: a state file the view's layers could not read: every layer that can
/// record one is asked.
pub(crate) fn state_corrupt(view: &DoctorView, _: &Selection, _: &mut Budget) -> Detected {
    view.registry();
    view.records();
    view.journals();
    view.lock();
    view.history();
    Detected {
        found: view.failure_observations(ids::STATE_CORRUPT),
        incomplete: None,
    }
}

/// `state-version-newer`: a target's record written by a newer omaddons, and a state file
/// a newer omaddons wrote that does not decode.
pub(crate) fn state_version_newer(view: &DoctorView, _: &Selection, _: &mut Budget) -> Detected {
    let mut found: Vec<Observation> = view
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
    view.registry();
    view.records();
    found.extend(view.failure_observations(ids::STATE_VERSION_NEWER));
    Detected {
        found,
        incomplete: None,
    }
}
