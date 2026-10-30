//! The facts of the seven failure modes `status` publishes, and each one's rule over
//! them: `status` gathers the facts from what it already computed and runs the rules
//! ([`findings`]); the doctor's detectors gather the same facts from the view's layers and
//! run the same rules, so the two never disagree.
//!
//! Temporary by design: F8 puts `status` on the registry's detectors and removes this
//! module.

use std::path::PathBuf;

use super::detectors::{concurrency, external, plugins, schemas, state_files, userland};
use super::finding::{Finding, Observation};
use super::registry::{FAILURE_MODES, Tier, ids};
use crate::drift::DriftState;
use crate::engine::journal::Unrecoverable;
use crate::model::{TargetKey, TxId};
use crate::state::store::SchemaVersion;

/// One failure mode's rule: its observations in the facts.
type Rule = fn(&StatusFacts) -> Vec<Observation>;

/// The rule of each failure mode `status` publishes.
const RULES: [(&str, Rule); 7] = [
    (ids::JOURNAL_PENDING, concurrency::journal_pending_in),
    (ids::STATE_VERSION_NEWER, schemas::state_version_newer_in),
    (
        ids::RECORD_FOLDER_MISSING,
        state_files::record_folder_missing_in,
    ),
    (
        ids::MANAGED_FILES_MODIFIED,
        external::managed_files_modified_in,
    ),
    (
        ids::OWNERSHIP_TRANSFERRED,
        external::ownership_transferred_in,
    ),
    (ids::TARGET_VANISHED, userland::target_vanished_in),
    (ids::WIDGET_VERSION_DRIFT, plugins::widget_version_drift_in),
];

/// The findings of the failure modes `status` publishes, up to `max_tier`, in registry
/// order: `status --cached` asks for the `cached` rows, plain `status` the `quick` ones
/// too.
pub fn findings(facts: &StatusFacts, max_tier: Tier) -> Vec<Finding> {
    FAILURE_MODES
        .iter()
        .filter(|row| row.tier <= max_tier)
        .filter_map(|row| {
            let (_, rule) = RULES.iter().find(|(id, _)| *id == row.id)?;
            Some((row, rule))
        })
        .flat_map(|(row, rule)| rule(facts).into_iter().map(|obs| row.finding(obs)))
        .collect()
}

/// Everything the seven detectors read.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StatusFacts {
    pub targets: Vec<TargetFacts>,
    pub machine: MachineFacts,
}

/// One registered target's facts. Each is set exactly when `status` raises the attention
/// item it stands for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetFacts {
    pub id: String,
    pub key: TargetKey,
    /// The pending journals naming it, oldest first, while no omaddons process holds the
    /// lock; otherwise none (`journal_pending`).
    pub journals: Vec<PendingJournal>,
    /// Its record, written by a newer omaddons (`state_version_newer`).
    pub newer_record: Option<NewerFile>,
    /// Its client directory, when that is gone (`target_missing`).
    pub vanished: Option<PathBuf>,
    /// Its degraded addons when `status` raises `addon_degraded`, else 0.
    pub degraded: usize,
    /// Its missing and modified addons, as `status` filtered them (under `--cached`,
    /// missing ones only).
    pub drift: Vec<AddonDriftFact>,
}

impl TargetFacts {
    /// A target with nothing to report.
    pub fn new(id: impl Into<String>, key: TargetKey) -> Self {
        Self {
            id: id.into(),
            key,
            journals: Vec::new(),
            newer_record: None,
            vanished: None,
            degraded: 0,
            drift: Vec::new(),
        }
    }
}

/// A pending journal and why recovery cannot settle it by itself, when it cannot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PendingJournal {
    pub tx: TxId,
    pub unrecoverable: Option<Unrecoverable>,
}

/// A state file a newer omaddons wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewerFile {
    pub path: PathBuf,
    /// The version on disk.
    pub on_disk: SchemaVersion,
    /// The version this build writes.
    pub writes: SchemaVersion,
}

/// A managed addon whose folders are gone or whose files changed on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddonDriftFact {
    pub addon: String,
    /// `missing` or `modified`.
    pub state: DriftState,
    /// Its changed files, across its folders.
    pub files: u64,
    /// The folders gone from `Interface/AddOns`.
    pub missing_folders: Vec<PathBuf>,
}

/// The facts of no client.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MachineFacts {
    /// Pending journals that name no client, while no omaddons process holds the lock.
    pub unattributed: Vec<PendingJournal>,
    /// The bar widget, when `status` asks for its reinstall.
    pub widget: Option<WidgetDrift>,
}

/// An outdated bar widget.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WidgetDrift {
    /// The version it was installed at, when it says.
    pub version: Option<String>,
}
