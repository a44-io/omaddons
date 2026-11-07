//! The detectors, one file per subsystem, so the catalog leaf that later owns a row
//! rewrites its detector in place. Each reads only the view and sets `confidence: high`.
//! A few rows gather their clients' [`TargetFacts`] from the view's layers and run a pure
//! rule over them (`*_in`), so the rule can be tested without a home.

pub(crate) mod caches;
pub(crate) mod concurrency;
pub(crate) mod configs;
pub(crate) mod external;
pub(crate) mod network;
pub(crate) mod plugins;
pub(crate) mod schemas;
pub(crate) mod secrets;
pub(crate) mod state_files;
#[cfg(test)]
mod tests;
pub(crate) mod userland;

use std::path::PathBuf;

use crate::app::drift::needs_reinstall;
use crate::doctor::selection::Selection;
use crate::doctor::view::DoctorView;
use crate::drift::{DriftMode, DriftState};
use crate::model::TargetKey;
use crate::state::store::SchemaVersion;

/// One registered client's facts, as a row's rule reads them: each is set exactly when
/// the client shows what the row reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TargetFacts {
    pub id: String,
    pub key: TargetKey,
    /// Its record, written by a newer omaddons (`state-version-newer`).
    pub newer_record: Option<NewerFile>,
    /// Its client directory, when that is gone (`target-vanished`).
    pub vanished: Option<PathBuf>,
    /// Its degraded addons (`ownership-transferred`).
    pub degraded: usize,
    /// Its missing and modified addons (`record-folder-missing`,
    /// `managed-files-modified`).
    pub drift: Vec<AddonDriftFact>,
}

impl TargetFacts {
    /// A client with nothing to report.
    pub fn new(id: impl Into<String>, key: TargetKey) -> Self {
        Self {
            id: id.into(),
            key,
            newer_record: None,
            vanished: None,
            degraded: 0,
            drift: Vec::new(),
        }
    }
}

/// A state file a newer omaddons wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NewerFile {
    pub path: PathBuf,
    /// The version on disk.
    pub on_disk: SchemaVersion,
    /// The version this build writes.
    pub writes: SchemaVersion,
}

/// A managed addon whose folders are gone or whose files changed on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AddonDriftFact {
    pub addon: String,
    /// `missing` or `modified`.
    pub state: DriftState,
    /// Its changed files, across its folders.
    pub files: u64,
    /// The folders gone from `Interface/AddOns`.
    pub missing_folders: Vec<PathBuf>,
}

/// Each registered discovered client in scope whose record was read, with its addons
/// that need a reinstall (missing or modified) as `mode` sees them: the facts of
/// `record-folder-missing` and `managed-files-modified`. The reports come from the view's
/// drift memo ([`DoctorView::drift`]), so each client's files are checked once a run, and
/// a deeper report a command seeded answers (`status --deep`).
pub(crate) fn drift_facts(view: &DoctorView, sel: &Selection, mode: DriftMode) -> Vec<TargetFacts> {
    let Some(world) = view.world() else {
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
            let report = view.drift(key, mode)?;
            let drift = report
                .addons
                .iter()
                .filter(|a| needs_reinstall(a.state))
                .map(|a| AddonDriftFact {
                    files: crate::app::drift::files(a),
                    missing_folders: a
                        .folders
                        .iter()
                        .filter(|f| !f.present)
                        .map(|f| d.target.addons_dir().join(f.folder.as_str()))
                        .collect(),
                    addon: a.addon.clone(),
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
