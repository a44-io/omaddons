//! `external_artifacts`: managed addons changed by hand or by another install.

use crate::doctor::budget::Budget;
use crate::doctor::facts::{StatusFacts, TargetFacts};
use crate::doctor::finding::{Confidence, Detected, Evidence, Observation, Subject};
use crate::doctor::selection::Selection;
use crate::doctor::view::DoctorView;
use crate::drift::{DriftMode, DriftState};
use crate::model::AddonState;

/// `managed-files-modified`, from the Discovery and Records layers: each managed file
/// checked stat-first, as plain `status` checks it.
pub(crate) fn managed_files_modified(
    view: &DoctorView,
    sel: &Selection,
    _: &mut Budget,
) -> Detected {
    let facts = StatusFacts {
        targets: super::drift_facts(view, sel, DriftMode::Stat),
        machine: Default::default(),
    };
    Detected::complete(managed_files_modified_in(&facts))
}

/// `managed-files-modified`'s rule: one finding per managed addon whose files changed on
/// disk.
pub(crate) fn managed_files_modified_in(facts: &StatusFacts) -> Vec<Observation> {
    facts
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
        .collect()
}

/// `ownership-transferred`, from the Records layer alone: each record with degraded
/// addons.
pub(crate) fn ownership_transferred(view: &DoctorView, _: &Selection, _: &mut Budget) -> Detected {
    let targets = view
        .records()
        .into_iter()
        .flat_map(|records| records.available())
        .map(|(key, loaded)| TargetFacts {
            degraded: loaded
                .value
                .addons
                .values()
                .filter(|addon| addon.state == AddonState::Degraded)
                .count(),
            ..TargetFacts::new(view.target_ref(*key).id, *key)
        })
        .collect();
    let facts = StatusFacts {
        targets,
        machine: Default::default(),
    };
    Detected::complete(ownership_transferred_in(&facts))
}

/// `ownership-transferred`'s rule: a target whose addons lost folders to other addons
/// (`--force`).
pub(crate) fn ownership_transferred_in(facts: &StatusFacts) -> Vec<Observation> {
    facts
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
        .collect()
}
