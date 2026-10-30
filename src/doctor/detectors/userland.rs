//! `userland_state`: clients the registry remembers.

use crate::doctor::budget::Budget;
use crate::doctor::facts::{StatusFacts, TargetFacts};
use crate::doctor::finding::{Confidence, Detected, Evidence, Observation, Subject};
use crate::doctor::selection::Selection;
use crate::doctor::view::DoctorView;

/// `target-vanished`, from the Discovery layer: each registered client discovery no
/// longer finds.
pub(crate) fn target_vanished(view: &DoctorView, _: &Selection, _: &mut Budget) -> Detected {
    let targets = view
        .world()
        .into_iter()
        .flat_map(|world| &world.resolved.missing)
        .map(|entry| TargetFacts {
            vanished: Some(entry.client_dir.clone()),
            ..TargetFacts::new(view.target_ref(entry.key).id, entry.key)
        })
        .collect();
    let facts = StatusFacts {
        targets,
        machine: Default::default(),
    };
    Detected {
        found: target_vanished_in(&facts),
        incomplete: None,
    }
}

/// `target-vanished`'s rule: a registered client whose directory is gone.
pub(crate) fn target_vanished_in(facts: &StatusFacts) -> Vec<Observation> {
    facts
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
        .collect()
}
