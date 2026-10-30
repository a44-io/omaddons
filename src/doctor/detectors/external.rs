//! `external_artifacts`: managed folders changed by something other than omaddons.

use crate::doctor::budget::Budget;
use crate::doctor::finding::{Confidence, Detected, Evidence, Observation, Subject};
use crate::doctor::selection::Selection;
use crate::doctor::view::DoctorView;
use crate::drift::DriftState;

/// `managed-files-modified`: one finding per managed addon whose files changed on disk.
pub(crate) fn managed_files_modified(view: &DoctorView, _: &Selection, _: &mut Budget) -> Detected {
    let found = view
        .status_facts()
        .targets
        .iter()
        .flat_map(|t| {
            t.drift
                .iter()
                .filter(|d| d.state == DriftState::Modified)
                .map(|d| {
                    let files = if d.files == 1 { "file" } else { "files" };
                    Observation {
                        subject: Subject::target(&t.id, t.key).addon(&d.addon),
                        title: format!("{}'s files changed on disk ({} {files})", d.addon, d.files),
                        confidence: Confidence::High,
                        addon: Some(d.addon.clone()),
                        folder: None,
                        evidence: Evidence {
                            count: Some(d.files),
                            ..Evidence::default()
                        },
                    }
                })
        })
        .collect();
    Detected {
        found,
        incomplete: None,
    }
}

/// `ownership-transferred`: a target whose addons lost folders to other addons
/// (`--force`).
pub(crate) fn ownership_transferred(view: &DoctorView, _: &Selection, _: &mut Budget) -> Detected {
    let found = view
        .status_facts()
        .targets
        .iter()
        .filter(|t| t.degraded > 0)
        .map(|t| {
            let n = t.degraded;
            let addons = if n == 1 { "addon" } else { "addons" };
            Observation {
                subject: Subject::target(&t.id, t.key),
                title: format!("{n} {addons} lost a folder to another addon"),
                confidence: Confidence::High,
                addon: None,
                folder: None,
                evidence: Evidence {
                    count: Some(n as u64),
                    ..Evidence::default()
                },
            }
        })
        .collect();
    Detected {
        found,
        incomplete: None,
    }
}
