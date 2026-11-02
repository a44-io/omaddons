//! The detectors, one file per subsystem, so the catalog leaf that later owns a row
//! rewrites its detector in place. Each reads only the view and sets `confidence: high`.
//! A row `status` publishes has a rule over [`crate::doctor::facts`] too, which its
//! detector feeds from the view's layers.

pub(crate) mod caches;
pub(crate) mod concurrency;
pub(crate) mod configs;
pub(crate) mod external;
pub(crate) mod network;
pub(crate) mod plugins;
pub(crate) mod schemas;
pub(crate) mod state_files;
#[cfg(test)]
mod tests;
pub(crate) mod userland;

use crate::app::drift::{needs_reinstall, report};
use crate::doctor::facts::{AddonDriftFact, TargetFacts};
use crate::doctor::selection::Selection;
use crate::doctor::view::DoctorView;
use crate::drift::DriftMode;

/// Each registered discovered client in scope whose record was read, with its addons
/// that need a reinstall (missing or modified) as `mode` sees them: the facts of
/// `record-folder-missing` and `managed-files-modified`.
pub(crate) fn drift_facts(view: &DoctorView, sel: &Selection, mode: DriftMode) -> Vec<TargetFacts> {
    let (Some(world), Some(records)) = (view.world(), view.records()) else {
        return Vec::new();
    };
    // A scope that does not resolve made the run fail before any detector ran.
    let scope = sel.scope(view).ok();
    world
        .resolved
        .targets
        .iter()
        .filter_map(|d| {
            let key = d.target.key?;
            if !scope
                .as_ref()
                .is_none_or(|scope| scope.includes_client(Some(key), &d.target.id))
            {
                return None;
            }
            let record = records.get(&key)?;
            let drift = report(view.paths(), &d.target, &record.value, mode)
                .addons
                .into_iter()
                .filter(|a| needs_reinstall(a.state))
                .map(|a| AddonDriftFact {
                    files: crate::app::drift::files(&a),
                    missing_folders: a
                        .folders
                        .iter()
                        .filter(|f| !f.present)
                        .map(|f| d.target.addons_dir().join(f.folder.as_str()))
                        .collect(),
                    addon: a.addon,
                    state: a.state,
                })
                .collect();
            Some(TargetFacts {
                drift,
                ..TargetFacts::new(view.target_ref(key).id, key)
            })
        })
        .collect()
}
