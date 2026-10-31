//! `plugins`: the bar widget.

use crate::app::status::machine_facts;
use crate::doctor::budget::Budget;
use crate::doctor::facts::StatusFacts;
use crate::doctor::finding::{Confidence, Detected, Evidence, Observation, Subject};
use crate::doctor::selection::Selection;
use crate::doctor::view::DoctorView;

/// `widget-version-drift`, from the Widget layer.
pub(crate) fn widget_version_drift(view: &DoctorView, _: &Selection, _: &mut Budget) -> Detected {
    let widget = view.widget().and_then(|layer| layer.info.as_ref());
    let facts = StatusFacts {
        targets: Vec::new(),
        machine: machine_facts(widget),
    };
    Detected::complete(widget_version_drift_in(&facts))
}

/// `widget-version-drift`'s rule: the bar widget serving this state is from another
/// build.
pub(crate) fn widget_version_drift_in(facts: &StatusFacts) -> Vec<Observation> {
    facts
        .machine
        .widget
        .iter()
        .map(|widget| Observation {
            subject: Subject::machine(),
            title: "The bar widget is from another omaddons build".to_owned(),
            confidence: Confidence::High,
            addon: None,
            folder: None,
            evidence: Evidence {
                observed: widget.version.clone(),
                ..Evidence::default()
            },
        })
        .collect()
}
