//! Drift as `status`, `list` and `info` show it ([`crate::drift`] finds it): per addon
//! its state and changed-file count, with each folder's paths under `-v`, and the
//! command that repairs it. Computed live, never stored (M2-D15).

use schemars::JsonSchema;
use serde::Serialize;

use crate::contract::actions::Action;
use crate::drift::{AddonDrift, DriftMode, DriftReport, DriftState, FolderDrift, check};
use crate::error::CommandHint;
use crate::model::{Target, TargetKey};
use crate::state::files_index::FilesIndex;
use crate::state::paths::Paths;
use crate::state::records::TargetRecord;
use crate::state::store;

/// One addon's drift, as `status` lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct DriftItem {
    pub addon: String,
    pub state: DriftState,
    /// Modified, deleted and added files across its folders.
    pub files: u64,
    /// `-v` only: each folder, with its paths.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub folders: Option<Vec<FolderDrift>>,
}

/// An addon's drift, as `list` and `info` show it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct DriftSummary {
    pub state: DriftState,
    pub files: u64,
    /// `-v` only: each folder, with its paths.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub folders: Option<Vec<FolderDrift>>,
}

impl DriftItem {
    pub fn of(addon: &AddonDrift, verbose: bool) -> Self {
        Self {
            addon: addon.addon.clone(),
            state: addon.state,
            files: files(addon),
            folders: verbose.then(|| addon.folders.clone()),
        }
    }
}

impl DriftSummary {
    pub fn of(addon: &AddonDrift, verbose: bool) -> Self {
        Self {
            state: addon.state,
            files: files(addon),
            folders: verbose.then(|| addon.folders.clone()),
        }
    }
}

/// The changed files across an addon's folders.
pub fn files(addon: &AddonDrift) -> u64 {
    addon.folders.iter().map(|f| f.changed).sum()
}

/// The target's files index; one that cannot be read is absent, so its folders come out
/// `unindexed` instead of failing the command.
pub fn index(paths: &Paths, key: &TargetKey) -> Option<FilesIndex> {
    store::load::<FilesIndex>(&paths.files_index(key))
        .ok()
        .flatten()
        .map(|loaded| loaded.value)
}

/// `target`'s drift in `mode`; `presence` reads no index.
pub fn report(
    paths: &Paths,
    target: &Target,
    record: &TargetRecord,
    mode: DriftMode,
) -> DriftReport {
    let index = match (mode, target.key) {
        (DriftMode::Presence, _) | (_, None) => None,
        (_, Some(key)) => index(paths, &key),
    };
    check(target, record, index.as_ref(), mode)
}

/// `omaddons install <addon> -t <id> --reinstall`: puts back what drifted.
pub fn reinstall_hint(addon: &str, target: &str) -> CommandHint {
    CommandHint::new(
        Action::Reinstall,
        ["omaddons", "install", addon, "-t", target, "--reinstall"],
    )
}

/// Whether an addon needs the repair: its files changed or a folder is gone.
pub const fn needs_reinstall(state: DriftState) -> bool {
    matches!(state, DriftState::Modified | DriftState::Missing)
}
