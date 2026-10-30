//! The bridge from what `status` already computes to the registry's detectors (E1): the
//! seven failure modes `status` publishes, as facts, so their detectors read exactly what
//! `status` raised attention for.
//!
//! Temporary by design. E2's `DoctorView::load` must fill these facts from its tolerant
//! layers, computed as `status` computes them, so that `doctor` (E3) reports the seven too;
//! each catalog leaf that moves one of the seven onto the layers drops that row's use of
//! them; F8 removes this module.

use std::path::PathBuf;

use crate::drift::DriftState;
use crate::engine::journal::Unrecoverable;
use crate::model::{TargetKey, TxId};
use crate::state::store::SchemaVersion;

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
