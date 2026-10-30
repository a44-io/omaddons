//! `plugins`: the bar widget.

use crate::doctor::budget::Budget;
use crate::doctor::finding::{Confidence, Detected, Evidence, Observation, Subject};
use crate::doctor::selection::Selection;
use crate::doctor::view::DoctorView;

/// `widget-version-drift`: the bar widget serving this state is from another build.
pub(crate) fn widget_version_drift(view: &DoctorView, _: &Selection, _: &mut Budget) -> Detected {
    let found = view
        .status_facts()
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
        .collect();
    Detected {
        found,
        incomplete: None,
    }
}
