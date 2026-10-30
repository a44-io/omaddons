//! The failure-mode registry (M4-D29): every failure mode declared once, with its id,
//! subsystem, severity, texts, tier, cost, the layers it reads, a pure detector, an
//! optional fixer and a manual remediation. `status`, `doctor` and `doctor health` all
//! read it, each up to its tier, so "is this broken?" has one implementation.
//!
//! The rows are sorted by id, and catalog leaves insert theirs where they sort.
//! [`invariant_violations`] holds every row to the registry's rules.

use std::collections::BTreeSet;

use schemars::JsonSchema;
use serde::Serialize;

use super::budget::Budget;
use super::detectors::{concurrency, configs, external, plugins, schemas, state_files, userland};
use super::finding::{
    Detected, Finding, Observation, Remediation, Severity, Subsystem, is_fm_id, subsystem_part,
};
use super::selection::Selection;
use super::view::{self, DoctorView};
use crate::app::drift::reinstall_hint;
use crate::app::status::{forget_target_hint, inspect_hint};
use crate::engine::repair_action::{Fix, FixKind};
use crate::error::CommandHint;
use crate::fsx::{Scope, Writer};
use crate::limits::QUICK_DETECTOR_BUDGET;
use crate::omarchy::status as omarchy_status;

/// The failure-mode ids (vision D20) that `status` publishes, stable across releases.
pub mod ids {
    /// An interrupted transaction waits for recovery.
    pub const JOURNAL_PENDING: &str = "fm-concurrency_primitives-journal-pending";
    /// State written by a newer omaddons.
    pub const STATE_VERSION_NEWER: &str = "fm-schemas-state-version-newer";
    /// A managed addon's folder is gone.
    pub const RECORD_FOLDER_MISSING: &str = "fm-state_files-record-folder-missing";
    /// A managed addon's files changed on disk.
    pub const MANAGED_FILES_MODIFIED: &str = "fm-external_artifacts-managed-files-modified";
    /// Addons lost a folder to another addon (`--force`).
    pub const OWNERSHIP_TRANSFERRED: &str = "fm-external_artifacts-ownership-transferred";
    /// A registered client's directory is gone.
    pub const TARGET_VANISHED: &str = "fm-userland_state-target-vanished";
    /// The bar widget is from another omaddons build (E4 raises it).
    pub const WIDGET_VERSION_DRIFT: &str = "fm-plugins-widget-version-drift";

    /// The seven ids above: the ones `status` publishes.
    pub(crate) const ALL: [&str; 7] = [
        JOURNAL_PENDING,
        STATE_VERSION_NEWER,
        RECORD_FOLDER_MISSING,
        MANAGED_FILES_MODIFIED,
        OWNERSHIP_TRANSFERRED,
        TARGET_VANISHED,
        WIDGET_VERSION_DRIFT,
    ];

    /// `config.toml` does not parse, holds a value out of range, or cannot be read (the
    /// doctor's view raises it).
    pub const CONFIG_PARSE_ERROR: &str = "fm-configs-config-parse-error";
    /// A state file cannot be read (the doctor's view raises it).
    pub const STATE_CORRUPT: &str = "fm-schemas-state-corrupt";
}

// ---------------------------------------------------------------------------------------
// The row type and its closed sets.

/// How costly a detector is, and so who runs it: `status --cached` runs `cached`;
/// `status`, `doctor --quick` and `doctor health` run `cached` and `quick`; `doctor` all
/// but `deep`; `doctor --include-deep` all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    /// Reads only what `status --cached` reads, plus single-file stats.
    Cached,
    /// Within `QUICK_DETECTOR_BUDGET`, offline.
    Quick,
    Default,
    Deep,
}

/// What a detector reads, so a tolerant view can say what it could not load.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Layer {
    Config,
    Registry,
    Discovery,
    Records,
    Inventory,
}

impl Layer {
    pub const ALL: [Self; 5] = [
        Self::Config,
        Self::Registry,
        Self::Discovery,
        Self::Records,
        Self::Inventory,
    ];

    /// The layers it is built from: it is unavailable when one of them is. Records never
    /// need the registry, so a corrupt `registry.json` leaves the records-only checks.
    pub const fn depends_on(self) -> &'static [Self] {
        match self {
            Self::Discovery => &[Self::Config, Self::Registry],
            Self::Inventory => &[Self::Discovery, Self::Records],
            Self::Config | Self::Registry | Self::Records => &[],
        }
    }
}

/// Where a fixer may write: a row of fsx's scope table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ScopeName {
    State,
    Config,
    Cache,
    ClientMeta,
    #[serde(rename = "addons")]
    AddOns,
    Skill,
}

impl ScopeName {
    /// Its row in fsx's scope table.
    pub const fn scope(self) -> Scope {
        match self {
            Self::State => Scope::State,
            Self::Config => Scope::Config,
            Self::Cache => Scope::Cache,
            Self::ClientMeta => Scope::ClientMeta,
            Self::AddOns => Scope::AddOns,
            Self::Skill => Scope::Skill,
        }
    }
}

/// How many paths a fix touches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Cardinality {
    /// At most 10 paths.
    Low,
    High,
}

/// What must hold before a fixer's plan runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FixerPrecondition {
    LockHeld,
    InflightFree,
    ClientNotRunning,
    Online,
    SameFs,
}

/// Why a fixer refuses (M4-D40): `doctor_refused`'s closed `details.reason`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RefusalReason {
    ForceRequired,
    FixerConflict,
    TooManyActions,
    OverBudget,
    FilesystemMismatch,
    ClientRunning,
    NotEnabled,
    EditedLocally,
}

impl RefusalReason {
    pub const ALL: [Self; 8] = [
        Self::ForceRequired,
        Self::FixerConflict,
        Self::TooManyActions,
        Self::OverBudget,
        Self::FilesystemMismatch,
        Self::ClientRunning,
        Self::NotEnabled,
        Self::EditedLocally,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ForceRequired => "force_required",
            Self::FixerConflict => "fixer_conflict",
            Self::TooManyActions => "too_many_actions",
            Self::OverBudget => "over_budget",
            Self::FilesystemMismatch => "filesystem_mismatch",
            Self::ClientRunning => "client_running",
            Self::NotEnabled => "not_enabled",
            Self::EditedLocally => "edited_locally",
        }
    }
}

/// A fixer's refusal to plan an instance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Refusal {
    pub reason: RefusalReason,
    pub message: String,
}

/// A row's detector: what it found in the view, and the limit that stopped it, if any.
pub type DetectFn = fn(&DoctorView, &Selection, &mut Budget) -> Detected;

/// A row's fixer.
#[derive(Debug, Clone, Copy)]
pub struct FixerSpec {
    /// The fixes that repair one finding, or why it refuses.
    pub plan: fn(&DoctorView, &Finding) -> Result<Vec<Fix>, Refusal>,
    pub writes_to: &'static [ScopeName],
    pub ops: &'static [FixKind],
    /// A fixer that changes user content ships disabled (M4-D39).
    pub enabled: bool,
    /// A set-aside runs only with `--force` (M4-D38).
    pub needs_force: bool,
    pub cardinality: Cardinality,
    pub preconditions: &'static [FixerPrecondition],
}

/// One failure mode.
#[derive(Debug, Clone, Copy)]
pub struct FailureMode {
    pub id: &'static str,
    pub subsystem: Subsystem,
    pub severity: Severity,
    pub title: &'static str,
    pub description: &'static str,
    pub why: &'static str,
    pub tier: Tier,
    /// It needs the network.
    pub online: bool,
    /// Its detector's estimated cost.
    pub cost_ms: u32,
    pub requires: &'static [Layer],
    /// `None` for a failure mode the runtime itself raises.
    pub detect: Option<DetectFn>,
    pub fix: Option<FixerSpec>,
    /// The remediation a person runs, when it is not (only) the fixer.
    pub manual: Option<fn(&Finding) -> Remediation>,
}

impl FailureMode {
    /// This row's finding for one observation.
    pub fn finding(&self, obs: Observation) -> Finding {
        Finding::build(self.id, self.subsystem, self.severity, obs, |f| {
            self.remediation(f)
        })
    }

    /// A finding's remediation, by row (M4-D53 refines it per instance): the manual one
    /// when the row has it, else `doctor fix --only <id>`; the `doctor explain` that goes
    /// with it; and whether the fixer runs by default.
    pub fn remediation(&self, f: &Finding) -> Remediation {
        let with_target = |mut argv: Vec<String>| {
            if let Some(target) = &f.target {
                argv.extend(["-t".to_owned(), target.id.clone()]);
            }
            argv
        };
        let base = match (self.manual, &self.fix) {
            (Some(manual), _) => manual(f),
            (None, Some(fix)) => {
                let mut argv =
                    with_target(argv_of(&["omaddons", "doctor", "fix", "--only", self.id]));
                if fix.needs_force {
                    argv.extend(argv_of(&["--force", "--yes"]));
                }
                Remediation {
                    command: shell_words::join(&argv),
                    argv: Some(argv),
                    ..Remediation::default()
                }
            }
            // Ruled out by the registry's invariants.
            (None, None) => Remediation::default(),
        };
        let explain_argv = with_target(argv_of(&["omaddons", "doctor", "explain", self.id]));
        Remediation {
            command: base.command,
            argv: base.argv,
            explain_command: shell_words::join(&explain_argv),
            explain_argv,
            auto_fixable: self.fix.is_some_and(|fix| fix.enabled && !fix.needs_force),
            needs_force: self.fix.is_some_and(|fix| fix.needs_force),
            estimated_actions: 0,
            destructive: base.destructive,
        }
    }
}

fn argv_of(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|part| (*part).to_owned()).collect()
}

/// The row of `id`.
pub fn row(id: &str) -> Option<&'static FailureMode> {
    FAILURE_MODES.iter().find(|row| row.id == id)
}

// ---------------------------------------------------------------------------------------
// The rows.

/// Every failure mode, sorted by id.
pub static FAILURE_MODES: &[FailureMode] = &[
    FailureMode {
        id: ids::JOURNAL_PENDING,
        subsystem: Subsystem::ConcurrencyPrimitives,
        severity: Severity::P0,
        title: "an interrupted change waits to be finished",
        description: "A change's journal is still in the state directory and no omaddons process holds the lock, so the change was interrupted between its first step and its commit.",
        why: "Every change on that client recovers it first; one that recovery cannot read blocks the client's changes until it is repaired.",
        tier: Tier::Cached,
        online: false,
        cost_ms: 1,
        requires: &[],
        detect: Some(concurrency::journal_pending),
        fix: None,
        manual: Some(journal_manual),
    },
    FailureMode {
        id: ids::CONFIG_PARSE_ERROR,
        subsystem: Subsystem::Configs,
        severity: Severity::P1,
        title: "config.toml cannot be read",
        description: "config.toml does not parse, holds a value out of range, is not UTF-8, is too large, or cannot be read.",
        why: "Every change command fails closed with state_corrupt until it is fixed, and the doctor skips every check that depends on the configuration.",
        tier: Tier::Cached,
        online: false,
        cost_ms: 1,
        requires: &[],
        detect: Some(configs::config_parse_error),
        fix: None,
        manual: Some(view_failure_manual),
    },
    FailureMode {
        id: ids::MANAGED_FILES_MODIFIED,
        subsystem: Subsystem::ExternalArtifacts,
        severity: Severity::P2,
        title: "a managed addon's files changed on disk",
        description: "Files of a folder omaddons installed differ from what its commit recorded: modified, deleted or added since.",
        why: "A reinstall or an update replaces them, so a hand edit would be lost, and an unintended change can break the addon.",
        tier: Tier::Quick,
        online: false,
        cost_ms: 5,
        requires: &[Layer::Records, Layer::Discovery],
        detect: Some(external::managed_files_modified),
        fix: None,
        manual: Some(reinstall_manual),
    },
    FailureMode {
        id: ids::OWNERSHIP_TRANSFERRED,
        subsystem: Subsystem::ExternalArtifacts,
        severity: Severity::P2,
        title: "addons lost a folder to another addon",
        description: "An install with --force gave a folder one addon owned to another, so the first addon is recorded as degraded.",
        why: "A degraded addon may not load, and updating it can take the folder back.",
        tier: Tier::Cached,
        online: false,
        cost_ms: 1,
        requires: &[Layer::Records],
        detect: Some(external::ownership_transferred),
        fix: None,
        manual: Some(inspect_manual),
    },
    FailureMode {
        id: ids::WIDGET_VERSION_DRIFT,
        subsystem: Subsystem::Plugins,
        severity: Severity::P3,
        title: "the bar widget is from another omaddons build",
        description: "The installed bar widget's code is not what this binary renders for the directories it was installed under.",
        why: "An outdated widget stays read-only and may misread this build's output.",
        tier: Tier::Cached,
        online: false,
        cost_ms: 1,
        requires: &[],
        detect: Some(plugins::widget_version_drift),
        fix: None,
        manual: Some(widget_manual),
    },
    FailureMode {
        id: ids::STATE_CORRUPT,
        subsystem: Subsystem::Schemas,
        severity: Severity::P0,
        title: "a state file cannot be read",
        description: "A state file (the target registry or a client's record) does not decode, is too large, is not a regular file or cannot be read, or the records directory holds more entries than omaddons ever writes.",
        why: "omaddons never rewrites a state file it cannot read, so every command that needs it fails closed with state_corrupt, and the doctor skips every check that depends on it.",
        tier: Tier::Default,
        online: false,
        cost_ms: 2,
        requires: &[],
        detect: Some(schemas::state_corrupt),
        fix: None,
        manual: Some(view_failure_manual),
    },
    FailureMode {
        id: ids::STATE_VERSION_NEWER,
        subsystem: Subsystem::Schemas,
        severity: Severity::P0,
        title: "state was written by a newer omaddons",
        description: "A client's record, or the target registry, carries a newer schema version than this build writes.",
        why: "This build never writes it, and cannot read one whose shape changed, so every change that needs it is refused until omaddons is upgraded.",
        tier: Tier::Cached,
        online: false,
        cost_ms: 1,
        requires: &[Layer::Records],
        detect: Some(schemas::state_version_newer),
        fix: None,
        manual: Some(upgrade_manual),
    },
    FailureMode {
        id: ids::RECORD_FOLDER_MISSING,
        subsystem: Subsystem::StateFiles,
        severity: Severity::P1,
        title: "a managed addon's folder is gone",
        description: "A folder the client's record says omaddons installed is missing from Interface/AddOns, or a symlink stands in its place.",
        why: "The addon no longer loads as recorded, and an update or a removal would act on a folder that is not there.",
        tier: Tier::Cached,
        online: false,
        cost_ms: 1,
        requires: &[Layer::Records, Layer::Discovery],
        detect: Some(state_files::record_folder_missing),
        fix: None,
        manual: Some(reinstall_manual),
    },
    FailureMode {
        id: ids::TARGET_VANISHED,
        subsystem: Subsystem::UserlandState,
        severity: Severity::P2,
        title: "a registered client's directory is gone",
        description: "A client in the target registry no longer has its directory on disk.",
        why: "Nothing can be installed there; its record and history are kept until the client is forgotten.",
        tier: Tier::Cached,
        online: false,
        cost_ms: 1,
        requires: &[Layer::Registry, Layer::Discovery],
        detect: Some(userland::target_vanished),
        fix: None,
        manual: Some(forget_manual),
    },
];

/// The manual remediations of the seven rows `status` publishes, built with `status`'s
/// own hints so a finding's remediation equals `status`'s `commands`.
fn journal_manual(f: &Finding) -> Remediation {
    // A journal on a target that recovery can read is finished by the next change; any
    // other needs manual repair.
    if f.target.is_some() && f.evidence.reason.is_none() {
        prose(
            "quit the game if it is running, then run any change command; it finishes or rolls back the change first",
        )
    } else {
        prose("see \"journal_pending that does not clear\" under Troubleshooting in the README")
    }
}

fn upgrade_manual(_: &Finding) -> Remediation {
    prose("upgrade omaddons")
}

/// A file the doctor's view could not read: prose, since no command repairs it.
fn view_failure_manual(f: &Finding) -> Remediation {
    prose(&view::failure_remediation(f.id, &f.evidence))
}

fn reinstall_manual(f: &Finding) -> Remediation {
    hint(&reinstall_hint(
        f.addon.as_deref().unwrap_or_default(),
        target_id(f),
    ))
}

fn inspect_manual(f: &Finding) -> Remediation {
    hint(&inspect_hint(target_id(f)))
}

fn forget_manual(f: &Finding) -> Remediation {
    hint(&forget_target_hint(target_id(f)))
}

fn widget_manual(_: &Finding) -> Remediation {
    hint(&omarchy_status::reinstall_hint())
}

/// The target id of a target's finding (always set on the rows that read it).
fn target_id(f: &Finding) -> &str {
    f.target.as_ref().map_or("", |target| target.id.as_str())
}

fn prose(text: &str) -> Remediation {
    Remediation {
        command: text.to_owned(),
        argv: None,
        ..Remediation::default()
    }
}

fn hint(hint: &CommandHint) -> Remediation {
    Remediation {
        command: hint.command().to_owned(),
        argv: Some(hint.argv().to_vec()),
        destructive: hint.destructive(),
        ..Remediation::default()
    }
}

// ---------------------------------------------------------------------------------------
// The invariants.

/// A rule a row breaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Rule {
    DuplicateId,
    NotSorted,
    BadId,
    SubsystemMismatch,
    InapplicableSubsystem,
    EmptyText,
    NoRemediation,
    ForcedFixerEnabled,
    OnlineInFastTier,
    TooCostlyForTier,
    FixerWithoutDetector,
    FixerWithoutOps,
    FixerWithoutScope,
    ScopeRefusesDoctor,
    IdWithoutRow,
}

/// One rule one row breaks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowViolation {
    pub id: String,
    pub rule: Rule,
}

/// Every rule each of `rows` breaks, in row order: ids unique, sorted and well formed,
/// matching an applicable subsystem; texts present, trimmed and single-line; a
/// remediation; a forced fixer never enabled; nothing online or over the quick budget in
/// the fast tiers; a fixer only with a detector, ops and a scope fsx lets the doctor
/// write.
pub fn invariant_violations(rows: &[FailureMode]) -> Vec<RowViolation> {
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    let mut previous: Option<&str> = None;
    for row in rows {
        let mut broken = |rule| {
            out.push(RowViolation {
                id: row.id.to_owned(),
                rule,
            });
        };
        if !seen.insert(row.id) {
            broken(Rule::DuplicateId);
        }
        if previous.is_some_and(|previous| previous > row.id) {
            broken(Rule::NotSorted);
        }
        previous = Some(row.id);
        if !is_fm_id(row.id) {
            broken(Rule::BadId);
        } else if subsystem_part(row.id) != Some(row.subsystem.as_str()) {
            broken(Rule::SubsystemMismatch);
        }
        if !row.subsystem.applicable() {
            broken(Rule::InapplicableSubsystem);
        }
        let well_formed =
            |text: &str| !text.is_empty() && text.trim() == text && !text.contains('\n');
        if ![row.title, row.description, row.why]
            .into_iter()
            .all(well_formed)
        {
            broken(Rule::EmptyText);
        }
        if row.fix.is_none() && row.manual.is_none() {
            broken(Rule::NoRemediation);
        }
        let fast = row.tier <= Tier::Quick;
        if row.online && fast {
            broken(Rule::OnlineInFastTier);
        }
        if fast && u128::from(row.cost_ms) > QUICK_DETECTOR_BUDGET.as_millis() {
            broken(Rule::TooCostlyForTier);
        }
        if let Some(fix) = &row.fix {
            if fix.needs_force && fix.enabled {
                broken(Rule::ForcedFixerEnabled);
            }
            if row.detect.is_none() {
                broken(Rule::FixerWithoutDetector);
            }
            if fix.ops.is_empty() {
                broken(Rule::FixerWithoutOps);
            } else if fix.writes_to.is_empty() && fix.ops != [FixKind::Delegate] {
                broken(Rule::FixerWithoutScope);
            }
            if fix
                .writes_to
                .iter()
                .any(|scope| !scope.scope().admits(Writer::Doctor))
            {
                broken(Rule::ScopeRefusesDoctor);
            }
        }
    }
    out
}

/// [`invariant_violations`] over [`FAILURE_MODES`], and every published id with its row.
pub fn registry_violations() -> Vec<RowViolation> {
    let mut out = invariant_violations(FAILURE_MODES);
    out.extend(
        ids::ALL
            .into_iter()
            .filter(|id| row(id).is_none())
            .map(|id| RowViolation {
                id: id.to_owned(),
                rule: Rule::IdWithoutRow,
            }),
    );
    out
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use serde_json::json;

    use super::*;
    use crate::doctor::finding::{Confidence, Evidence, FindingTarget, Subject};
    use crate::engine::repair_action::Mode;
    use crate::model::{TargetKey, TxId};

    const EXAMPLE: &str = "fm-state_files-example-mode";
    const K: &str = "01M5ZQ7Y3KJ8V9W2X4C6B0N1RT";

    fn nothing(_: &DoctorView, _: &Selection, _: &mut Budget) -> Detected {
        Detected::default()
    }

    fn no_fixes(_: &DoctorView, _: &Finding) -> Result<Vec<Fix>, Refusal> {
        Ok(Vec::new())
    }

    fn manual(_: &Finding) -> Remediation {
        prose("do the thing by hand")
    }

    /// A valid row, the template every bad row breaks one rule of.
    fn template() -> FailureMode {
        FailureMode {
            id: EXAMPLE,
            subsystem: Subsystem::StateFiles,
            severity: Severity::P2,
            title: "an example",
            description: "An example failure mode.",
            why: "It shows each rule.",
            tier: Tier::Default,
            online: false,
            cost_ms: 1,
            requires: &[],
            detect: Some(nothing),
            fix: None,
            manual: Some(manual),
        }
    }

    fn fixer() -> FixerSpec {
        FixerSpec {
            plan: no_fixes,
            writes_to: &[ScopeName::State],
            ops: &[FixKind::Write],
            enabled: true,
            needs_force: false,
            cardinality: Cardinality::Low,
            preconditions: &[FixerPrecondition::LockHeld],
        }
    }

    fn with(edit: impl FnOnce(&mut FailureMode)) -> FailureMode {
        let mut row = template();
        edit(&mut row);
        row
    }

    fn with_fixer(edit: impl FnOnce(&mut FixerSpec)) -> FailureMode {
        let mut fix = fixer();
        edit(&mut fix);
        with(|row| row.fix = Some(fix))
    }

    #[test]
    fn failure_modes_hold_their_invariants() {
        let violations = registry_violations();
        assert!(violations.is_empty(), "{violations:#?}");
    }

    #[test]
    fn each_invariant_fires_on_its_bad_row() {
        // Sorts before the template's id.
        let other = with(|row| row.id = "fm-state_files-another-mode");
        let cases: Vec<(&str, Vec<FailureMode>, Vec<Rule>)> = vec![
            ("valid", vec![template()], vec![]),
            (
                "duplicate_id",
                vec![template(), template()],
                vec![Rule::DuplicateId],
            ),
            ("not_sorted", vec![template(), other], vec![Rule::NotSorted]),
            (
                "bad_id",
                vec![with(|r| r.id = "fm-State-x")],
                vec![Rule::BadId],
            ),
            (
                "subsystem_mismatch",
                vec![with(|r| r.id = "fm-configs-x")],
                vec![Rule::SubsystemMismatch],
            ),
            (
                "inapplicable_subsystem",
                vec![with(|r| {
                    r.id = "fm-sockets-x";
                    r.subsystem = Subsystem::Sockets;
                })],
                vec![Rule::InapplicableSubsystem],
            ),
            (
                "empty_title",
                vec![with(|r| r.title = "")],
                vec![Rule::EmptyText],
            ),
            (
                "untrimmed_description",
                vec![with(|r| r.description = " An example. ")],
                vec![Rule::EmptyText],
            ),
            (
                "two_line_why",
                vec![with(|r| r.why = "One.\nTwo.")],
                vec![Rule::EmptyText],
            ),
            (
                "no_remediation",
                vec![with(|r| r.manual = None)],
                vec![Rule::NoRemediation],
            ),
            (
                "forced_fixer_enabled",
                vec![with_fixer(|f| f.needs_force = true)],
                vec![Rule::ForcedFixerEnabled],
            ),
            (
                "online_in_cached",
                vec![with(|r| {
                    r.online = true;
                    r.tier = Tier::Cached;
                })],
                vec![Rule::OnlineInFastTier],
            ),
            (
                "online_in_quick",
                vec![with(|r| {
                    r.online = true;
                    r.tier = Tier::Quick;
                })],
                vec![Rule::OnlineInFastTier],
            ),
            ("online_in_default", vec![with(|r| r.online = true)], vec![]),
            (
                "too_costly_for_quick",
                vec![with(|r| {
                    r.tier = Tier::Quick;
                    r.cost_ms = 6;
                })],
                vec![Rule::TooCostlyForTier],
            ),
            ("costly_in_default", vec![with(|r| r.cost_ms = 600)], vec![]),
            (
                "fixer_without_detector",
                vec![with(|r| {
                    r.detect = None;
                    r.fix = Some(fixer());
                })],
                vec![Rule::FixerWithoutDetector],
            ),
            (
                "fixer_without_ops",
                vec![with_fixer(|f| f.ops = &[])],
                vec![Rule::FixerWithoutOps],
            ),
            (
                "fixer_without_scope",
                vec![with_fixer(|f| f.writes_to = &[])],
                vec![Rule::FixerWithoutScope],
            ),
            (
                "delegate_only_needs_no_scope",
                vec![with_fixer(|f| {
                    f.writes_to = &[];
                    f.ops = &[FixKind::Delegate];
                })],
                vec![],
            ),
            (
                "a_fixer_alone_remediates",
                vec![with(|r| {
                    r.manual = None;
                    r.fix = Some(fixer());
                })],
                vec![],
            ),
        ];
        for (case, rows, expected) in cases {
            let rules: Vec<Rule> = invariant_violations(&rows)
                .into_iter()
                .map(|v| v.rule)
                .collect();
            assert_eq!(rules, expected, "{case}");
        }
        // `ScopeRefusesDoctor` cannot be built through `ScopeName`: every name admits the
        // doctor.
        for name in [
            ScopeName::State,
            ScopeName::Config,
            ScopeName::Cache,
            ScopeName::ClientMeta,
            ScopeName::AddOns,
            ScopeName::Skill,
        ] {
            assert!(name.scope().admits(Writer::Doctor), "{name:?}");
        }
    }

    #[test]
    fn the_seven_published_ids_are_registry_rows() {
        use crate::app::issues::ids as status_ids;
        let cases = [
            (
                ids::JOURNAL_PENDING,
                status_ids::JOURNAL_PENDING,
                Severity::P0,
                Tier::Cached,
            ),
            (
                ids::STATE_VERSION_NEWER,
                status_ids::STATE_VERSION_NEWER,
                Severity::P0,
                Tier::Cached,
            ),
            (
                ids::RECORD_FOLDER_MISSING,
                status_ids::RECORD_FOLDER_MISSING,
                Severity::P1,
                Tier::Cached,
            ),
            (
                ids::MANAGED_FILES_MODIFIED,
                status_ids::MANAGED_FILES_MODIFIED,
                Severity::P2,
                Tier::Quick,
            ),
            (
                ids::OWNERSHIP_TRANSFERRED,
                status_ids::OWNERSHIP_TRANSFERRED,
                Severity::P2,
                Tier::Cached,
            ),
            (
                ids::TARGET_VANISHED,
                status_ids::TARGET_VANISHED,
                Severity::P2,
                Tier::Cached,
            ),
            (
                ids::WIDGET_VERSION_DRIFT,
                status_ids::WIDGET_VERSION_DRIFT,
                Severity::P3,
                Tier::Cached,
            ),
        ];
        for (id, status_id, severity, tier) in cases {
            assert_eq!(id, status_id);
            let row = row(id);
            assert_eq!(
                row.map(|r| (r.severity, r.tier, r.online, r.fix.is_none())),
                Some((severity, tier, false, true)),
                "{id}"
            );
        }
        // The seven, and the two the doctor's view raises.
        assert_eq!(FAILURE_MODES.len(), 9);
    }

    #[test]
    fn view_reported_rows_match_appendix_b() {
        use crate::doctor::view::{LayerFailure, LayerState, Preloaded, Records};
        use crate::state::config::LoadedConfig;
        use crate::state::registry::Registry;
        use crate::state::store::{Loaded, VersionStatus, Versioned};

        let cases = [
            (
                ids::CONFIG_PARSE_ERROR,
                Subsystem::Configs,
                Severity::P1,
                Tier::Cached,
                1,
            ),
            (
                ids::STATE_CORRUPT,
                Subsystem::Schemas,
                Severity::P0,
                Tier::Default,
                2,
            ),
        ];
        for (id, subsystem, severity, tier, cost_ms) in cases {
            let row = row(id).unwrap();
            assert_eq!(
                (
                    row.subsystem,
                    row.severity,
                    row.tier,
                    row.online,
                    row.cost_ms,
                    row.requires,
                    row.fix.is_none(),
                    row.manual.is_some(),
                ),
                (
                    subsystem,
                    severity,
                    tier,
                    false,
                    cost_ms,
                    &[][..],
                    true,
                    true
                ),
                "{id}"
            );
        }

        // Every layer available, so a detector reads nothing more.
        let healthy = || Preloaded {
            config: Some(LayerState::Available(LoadedConfig::default())),
            registry: Some(LayerState::Available(Loaded {
                value: Registry::default(),
                version: Registry::CURRENT,
                status: VersionStatus::Current,
            })),
            records: Some(LayerState::Available(Records::default())),
            failures: Vec::new(),
        };
        let paths = crate::doctor::tests::fixture_paths();
        let failure = |fm: &'static str, file: &str, reason: &'static str| LayerFailure {
            layer: Layer::Config,
            fm,
            target: None,
            path: Path::new("/fixture").join(file),
            evidence: Evidence {
                path: Some(Path::new("/fixture").join(file)),
                reason: Some(reason),
                ..Evidence::default()
            },
            title: format!("{file} is broken"),
        };
        let failures = vec![
            failure(ids::CONFIG_PARSE_ERROR, "config.toml", "parse"),
            failure(ids::STATE_CORRUPT, "registry.json", "decode"),
            failure(ids::STATE_VERSION_NEWER, "targets/x.json", "newer"),
        ];
        let broken = DoctorView::preloaded(
            paths.clone(),
            Preloaded {
                failures,
                ..healthy()
            },
        );
        let healthy = DoctorView::preloaded(paths, healthy());
        let view_findings = broken.failure_findings();
        for id in [
            ids::CONFIG_PARSE_ERROR,
            ids::STATE_CORRUPT,
            ids::STATE_VERSION_NEWER,
        ] {
            let row = row(id).unwrap();
            let detect = row.detect.unwrap();
            let found: Vec<Finding> = detect(&broken, &Selection::all(), &mut Budget::unlimited())
                .found
                .into_iter()
                .map(|obs| row.finding(obs))
                .collect();
            let expected: Vec<Finding> = view_findings
                .iter()
                .filter(|f| f.id == id)
                .cloned()
                .collect();
            assert_eq!(found.len(), 1, "{id}: {found:?}");
            assert_eq!(
                found, expected,
                "{id}: the detector's finding is the view's"
            );
            let none = detect(&healthy, &Selection::all(), &mut Budget::unlimited());
            assert_eq!(none, Detected::default(), "{id} on a healthy view");
        }
        // A failure's remediation is the view's prose, and no command.
        for finding in &view_findings {
            assert_eq!(finding.remediation.argv, None, "{}", finding.id);
            assert!(!finding.remediation.command.is_empty(), "{}", finding.id);
        }
    }

    #[test]
    fn layer_order_and_dependencies() {
        assert_eq!(
            Layer::ALL,
            [
                Layer::Config,
                Layer::Registry,
                Layer::Discovery,
                Layer::Records,
                Layer::Inventory
            ]
        );
        let cases: [(Layer, &[Layer]); 5] = [
            (Layer::Config, &[]),
            (Layer::Registry, &[]),
            (Layer::Discovery, &[Layer::Config, Layer::Registry]),
            (Layer::Records, &[]),
            (Layer::Inventory, &[Layer::Discovery, Layer::Records]),
        ];
        for (layer, expected) in cases {
            assert_eq!(layer.depends_on(), expected, "{layer:?}");
        }
        // No cycle: a depth-first walk from each layer never comes back to it.
        for start in Layer::ALL {
            let mut stack: Vec<Layer> = start.depends_on().to_vec();
            let mut seen = BTreeSet::new();
            while let Some(layer) = stack.pop() {
                assert_ne!(layer, start, "{start:?} depends on itself");
                if seen.insert(layer) {
                    stack.extend(layer.depends_on());
                }
            }
        }
    }

    #[test]
    fn no_failure_mode_id_literal_outside_the_registry() {
        use crate::source_scan::{Literals, production_code, sources};
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut found = Vec::new();
        for (path, text) in sources(&root.join("src")) {
            if path.ends_with("src/doctor/registry.rs") {
                continue;
            }
            let Some(code) = production_code(&text, Literals::Keep) else {
                continue;
            };
            for (n, line) in code.lines().enumerate() {
                // An id literal: `"fm-` and an id's first character (the bare prefix
                // `"fm-"` that parses ids is no id).
                let id_literal = line.match_indices("\"fm-").any(|(at, _)| {
                    line[at + 4..]
                        .chars()
                        .next()
                        .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
                });
                if id_literal {
                    found.push(format!("{}:{}: {}", path.display(), n + 1, line.trim()));
                }
            }
        }
        assert!(
            found.is_empty(),
            "failure-mode ids outside the registry:\n{}",
            found.join("\n")
        );
    }

    fn finding(row: &FailureMode, target: bool) -> Finding {
        let subject = if target {
            Subject::target("retail", K.parse::<TargetKey>().unwrap())
        } else {
            Subject::machine()
        };
        row.finding(Observation {
            subject,
            title: "t".to_owned(),
            confidence: Confidence::High,
            addon: None,
            folder: None,
            evidence: Evidence::default(),
        })
    }

    #[test]
    fn remediations_by_case() {
        let explain = |target: bool| {
            let mut argv = argv_of(&["omaddons", "doctor", "explain", EXAMPLE]);
            if target {
                argv.extend(argv_of(&["-t", "retail"]));
            }
            argv
        };
        let manual_row = template();
        let on_target = finding(&manual_row, true).remediation;
        assert_eq!(
            (
                on_target.command.as_str(),
                on_target.argv.as_ref(),
                &on_target.explain_argv
            ),
            ("do the thing by hand", None, &explain(true)),
            "manual_on_a_target"
        );
        assert_eq!(
            on_target.explain_command,
            "omaddons doctor explain fm-state_files-example-mode -t retail"
        );
        let on_machine = finding(&manual_row, false).remediation;
        assert_eq!(
            on_machine.explain_argv,
            explain(false),
            "manual_on_the_machine"
        );
        let fix = |edit: fn(&mut FixerSpec)| {
            let mut row = with_fixer(edit);
            row.manual = None;
            finding(&row, true).remediation
        };
        let cases: [(&str, Remediation, &[&str], bool, bool); 3] = [
            (
                "fixer",
                fix(|_| {}),
                &[
                    "omaddons", "doctor", "fix", "--only", EXAMPLE, "-t", "retail",
                ],
                true,
                false,
            ),
            (
                "forced_fixer",
                fix(|f| {
                    f.needs_force = true;
                    f.enabled = false;
                }),
                &[
                    "omaddons", "doctor", "fix", "--only", EXAMPLE, "-t", "retail", "--force",
                    "--yes",
                ],
                false,
                true,
            ),
            (
                "disabled_fixer",
                fix(|f| f.enabled = false),
                &[
                    "omaddons", "doctor", "fix", "--only", EXAMPLE, "-t", "retail",
                ],
                false,
                false,
            ),
        ];
        for (case, remediation, argv, auto_fixable, needs_force) in cases {
            assert_eq!(remediation.argv, Some(argv_of(argv)), "{case}");
            assert_eq!(remediation.command, shell_words::join(argv), "{case}");
            assert_eq!(
                (
                    remediation.auto_fixable,
                    remediation.needs_force,
                    remediation.estimated_actions
                ),
                (auto_fixable, needs_force, 0),
                "{case}"
            );
            assert_eq!(remediation.explain_argv, explain(true), "{case}");
        }
    }

    #[test]
    fn the_rows_remediations_are_statuss_hints() {
        let target = FindingTarget {
            id: "retail".to_owned(),
            key: K.parse().unwrap(),
        };
        let tx: TxId = "01M7A1B2C3D4E5F6G7H8J9K0MN".parse().unwrap();
        let journal = |reason: Option<&'static str>, on_target: bool| {
            let mut f = finding(row(ids::JOURNAL_PENDING).unwrap(), on_target);
            f.evidence = Evidence {
                tx: Some(tx),
                reason,
                ..Evidence::default()
            };
            journal_manual(&f).command
        };
        assert!(journal(None, true).starts_with("quit the game"));
        assert!(journal(Some("journal_too_new"), true).starts_with("see \"journal_pending"));
        assert!(journal(None, false).starts_with("see \"journal_pending"));
        let mut f = finding(row(ids::RECORD_FOLDER_MISSING).unwrap(), true);
        f.addon = Some("Details".to_owned());
        f.target = Some(target);
        assert_eq!(
            reinstall_manual(&f).argv,
            Some(argv_of(&[
                "omaddons",
                "install",
                "Details",
                "-t",
                "retail",
                "--reinstall"
            ]))
        );
        let forget = forget_manual(&f);
        assert_eq!(
            forget.argv,
            Some(argv_of(&["omaddons", "targets", "rm", "retail"]))
        );
        assert!(forget.destructive);
    }

    #[test]
    fn closed_sets_serialize_snake_case() {
        assert_eq!(
            json!([Tier::Cached, Tier::Quick, Tier::Default, Tier::Deep]),
            json!(["cached", "quick", "default", "deep"])
        );
        assert!(
            Tier::Cached < Tier::Quick && Tier::Quick < Tier::Default && Tier::Default < Tier::Deep
        );
        assert_eq!(
            json!([
                Layer::Config,
                Layer::Registry,
                Layer::Discovery,
                Layer::Records,
                Layer::Inventory
            ]),
            json!(["config", "registry", "discovery", "records", "inventory"])
        );
        assert_eq!(
            json!([
                ScopeName::State,
                ScopeName::Config,
                ScopeName::Cache,
                ScopeName::ClientMeta,
                ScopeName::AddOns,
                ScopeName::Skill
            ]),
            json!(["state", "config", "cache", "client_meta", "addons", "skill"])
        );
        assert_eq!(
            json!([Cardinality::Low, Cardinality::High]),
            json!(["low", "high"])
        );
        assert_eq!(
            json!([
                FixerPrecondition::LockHeld,
                FixerPrecondition::InflightFree,
                FixerPrecondition::ClientNotRunning,
                FixerPrecondition::Online,
                FixerPrecondition::SameFs
            ]),
            json!([
                "lock_held",
                "inflight_free",
                "client_not_running",
                "online",
                "same_fs"
            ])
        );
        for reason in RefusalReason::ALL {
            assert_eq!(json!(reason), json!(reason.as_str()), "{reason:?}");
        }
        assert_eq!(
            RefusalReason::ALL.map(RefusalReason::as_str),
            [
                "force_required",
                "fixer_conflict",
                "too_many_actions",
                "over_budget",
                "filesystem_mismatch",
                "client_running",
                "not_enabled",
                "edited_locally"
            ]
        );
    }

    #[test]
    fn evidence_mode_matches_the_primitive_mode_text() {
        for bits in [0o600, 0o644, 0o4755] {
            let evidence = Evidence {
                mode: Some(bits),
                ..Evidence::default()
            };
            assert_eq!(
                serde_json::to_value(&evidence).unwrap()["mode"],
                serde_json::to_value(Mode::new(bits).unwrap()).unwrap(),
                "{bits:o}"
            );
        }
    }
}
