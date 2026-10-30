//! `schemas`: state this build cannot read or write.

use crate::doctor::budget::Budget;
use crate::doctor::facts::{NewerFile, StatusFacts, TargetFacts};
use crate::doctor::finding::{Confidence, Detected, Evidence, Observation, Subject};
use crate::doctor::registry::ids;
use crate::doctor::selection::Selection;
use crate::doctor::view::DoctorView;
use crate::state::records::TargetRecord;
use crate::state::store::{VersionStatus, Versioned};

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

/// `state-version-newer`, from the Records layer: each record a newer omaddons wrote,
/// and each state file a newer omaddons wrote that does not decode (the view's
/// failures).
pub(crate) fn state_version_newer(view: &DoctorView, _: &Selection, _: &mut Budget) -> Detected {
    let targets = view
        .records()
        .into_iter()
        .flat_map(|records| records.available())
        .filter(|(_, loaded)| loaded.status == VersionStatus::Newer)
        .map(|(key, loaded)| TargetFacts {
            newer_record: Some(NewerFile {
                path: view.paths().target_record(key),
                on_disk: loaded.version,
                writes: <TargetRecord as Versioned>::CURRENT,
            }),
            ..TargetFacts::new(view.target_ref(*key).id, *key)
        })
        .collect();
    let mut found = state_version_newer_in(&StatusFacts {
        targets,
        machine: Default::default(),
    });
    view.registry();
    found.extend(view.failure_observations(ids::STATE_VERSION_NEWER));
    Detected {
        found,
        incomplete: None,
    }
}

/// `state-version-newer`'s rule: a target's record written by a newer omaddons.
pub(crate) fn state_version_newer_in(facts: &StatusFacts) -> Vec<Observation> {
    facts
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
        .collect()
}
