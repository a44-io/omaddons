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
use super::detectors::{
    caches, concurrency, configs, external, network, plugins, schemas, secrets, state_files,
    userland,
};
use super::finding::{
    Detected, Finding, Observation, Remediation, SecretOrigin, Severity, Subsystem, is_fm_id,
    subsystem_part,
};
use super::selection::Selection;
use super::view::{self, DoctorView};
use crate::app::drift::reinstall_hint;
use crate::app::status::{forget_target_hint, inspect_hint};
use crate::engine::repair_action::{Fix, FixKind};
use crate::error::CommandHint;
use crate::fsx::{Scope, Writer};
use crate::limits::{DOCTOR_SCAN_MAX_ENTRIES, QUICK_DETECTOR_BUDGET};
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
    /// A credential file other users can read.
    // ubs:ignore — a failure-mode id, not a secret
    pub const CREDENTIAL_FILE_MODE: &str = "fm-secrets-credential-file-mode";

    /// A check panicked during a doctor run (the runtime raises it).
    pub const DETECTOR_CRASHED: &str = "fm-state_files-detector-crashed";
    /// A check stopped at its deadline or entry cap (the runtime raises it).
    pub const CHECK_INCOMPLETE: &str = "fm-state_files-check-incomplete";

    /// An interrupted change to `WTF/` waits for its game to exit.
    pub const JOURNAL_WAITS_FOR_GAME: &str = "fm-concurrency_primitives-journal-waits-for-game";
    /// An interrupted change was started by a newer omaddons.
    pub const JOURNAL_TOO_NEW: &str = "fm-concurrency_primitives-journal-too-new";
    /// An interrupted change's journal cannot be read.
    pub const JOURNAL_UNREADABLE: &str = "fm-concurrency_primitives-journal-unreadable";
    /// An interrupted change to `WTF/` stopped at a state it does not recognize.
    pub const JOURNAL_UNEXPECTED_STATE: &str = "fm-concurrency_primitives-journal-unexpected-state";
    /// An interrupted change stopped at a step it does not recognize.
    pub const JOURNAL_STEP_UNRECOGNIZED: &str =
        "fm-concurrency_primitives-journal-step-unrecognized";
    /// An interrupted change names a directory that is not its own.
    pub const JOURNAL_MARKER_MISMATCH: &str = "fm-concurrency_primitives-journal-marker-mismatch";
    /// An interrupted change cannot be attributed to any client.
    pub const JOURNAL_UNATTRIBUTED: &str = "fm-concurrency_primitives-journal-unattributed";
    /// An interrupted restore has not finished keeping what it displaced.
    pub const JOURNAL_PRESERVATION_INCOMPLETE: &str =
        "fm-concurrency_primitives-journal-preservation-incomplete";
    /// The global lock has been held for longer than `LOCK_HELD_LONG`.
    pub const LOCK_HELD_LONG: &str = "fm-concurrency_primitives-lock-held-long";
    /// A cached download's bytes no longer hash to its name, or it cannot be read.
    pub const BLOB_HASH_MISMATCH: &str = "fm-caches-blob-hash-mismatch";

    /// The failure mode a recovery stop reports as (E10 points each stop error's fix at
    /// its `doctor explain`).
    pub fn for_stop_reason(reason: crate::engine::journal::StopReason) -> &'static str {
        use crate::engine::journal::StopReason;
        match reason {
            StopReason::SavedVariables => JOURNAL_WAITS_FOR_GAME,
            StopReason::JournalTooNew => JOURNAL_TOO_NEW,
            StopReason::JournalUnreadable => JOURNAL_UNREADABLE,
            StopReason::UnexpectedState => JOURNAL_UNEXPECTED_STATE,
            StopReason::StepUnrecognized => JOURNAL_STEP_UNRECOGNIZED,
            StopReason::MarkerMismatch | StopReason::SkillDir => JOURNAL_MARKER_MISMATCH,
            StopReason::Unattributed => JOURNAL_UNATTRIBUTED,
            StopReason::PreservationIncomplete => JOURNAL_PRESERVATION_INCOMPLETE,
        }
    }

    /// Several addons of one client's record claim one AddOns folder.
    pub const OWNERSHIP_CONFLICT: &str = "fm-external_artifacts-ownership-conflict";
    /// AddOns entries whose names differ only in case.
    pub const CASE_COLLISION: &str = "fm-external_artifacts-case-collision";
    /// A managed addon's folder no longer loads on its client.
    pub const WRONG_FLAVOR: &str = "fm-external_artifacts-wrong-flavor";
    /// A managed addon loads only as out of date.
    pub const OUT_OF_DATE_TOC: &str = "fm-external_artifacts-out-of-date-toc";

    /// The installed bar widget fails Omarchy's plugin validation.
    pub const WIDGET_INVALID: &str = "fm-plugins-widget-invalid";
    /// An installed agent skill holds another guide than this build's, or was edited.
    pub const SKILL_VERSION_DRIFT: &str = "fm-plugins-skill-version-drift";

    /// GitHub answers 401 to the configured token, or it cannot be had (`--online`).
    // ubs:ignore — a failure-mode id, not a secret
    pub const NETWORK_CREDENTIAL_REJECTED: &str = "fm-network-credential-rejected";
    /// GitHub's core REST budget is spent (`--online`).
    pub const NETWORK_GITHUB_RATE_LIMITED: &str = "fm-network-github-rate-limited";
    /// An enabled source an installed addon comes from does not answer (`--online`).
    pub const NETWORK_SOURCE_UNREACHABLE: &str = "fm-network-source-unreachable";

    /// The test-only failure modes (debug builds with `OMADDONS_TEST_FIXERS=1`,
    /// `doctor::test_fixers`): an ordinary auto-fixer, a forced one and a disabled one.
    #[cfg(debug_assertions)]
    pub const TEST_WRITE: &str = "fm-test-write";
    #[cfg(debug_assertions)]
    pub const TEST_QUARANTINE: &str = "fm-test-quarantine";
    #[cfg(debug_assertions)]
    pub const TEST_FLATTEN: &str = "fm-test-flatten";
    /// E7's: a chmod, and a guarded set-aside of a planted journal.
    #[cfg(debug_assertions)]
    pub const TEST_CHMOD: &str = "fm-test-chmod";
    #[cfg(debug_assertions)]
    pub const TEST_SUSPEND: &str = "fm-test-suspend";
}

// ---------------------------------------------------------------------------------------
// The row type and its closed sets.

/// How costly a detector is, and so who runs it: `status --cached` runs `cached`;
/// `status`, `doctor --quick` and `doctor health` run `cached` and `quick`; `doctor` all
/// but `deep`; `doctor --include-deep` all. `runtime` rows have no detector: the doctor
/// runtime raises them about its own run, and no tier filter selects them (it sorts
/// last, past every `max_tier`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    /// Reads only what `status --cached` reads, plus single-file stats.
    Cached,
    /// Within `QUICK_DETECTOR_BUDGET`, offline.
    Quick,
    Default,
    Deep,
    Runtime,
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
    Journals,
    Lock,
    History,
    Caches,
    Widget,
    Skill,
    Credentials,
}

impl Layer {
    pub const ALL: [Self; 12] = [
        Self::Config,
        Self::Registry,
        Self::Discovery,
        Self::Records,
        Self::Inventory,
        Self::Journals,
        Self::Lock,
        Self::History,
        Self::Caches,
        Self::Widget,
        Self::Skill,
        Self::Credentials,
    ];

    /// The layers it is built from: it is unavailable when one of them is. Records never
    /// need the registry, so a corrupt `registry.json` leaves the records-only checks.
    pub const fn depends_on(self) -> &'static [Self] {
        match self {
            Self::Discovery => &[Self::Config, Self::Registry],
            Self::Inventory => &[Self::Discovery, Self::Records],
            Self::Credentials => &[Self::Config],
            Self::Config
            | Self::Registry
            | Self::Records
            | Self::Journals
            | Self::Lock
            | Self::History
            | Self::Caches
            | Self::Widget
            | Self::Skill => &[],
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

/// What a person does about a failure mode, when it is not (only) a fixer's to repair: in
/// general, and why it is theirs to do (`doctor explain`'s `manual`), then the remediation
/// of one finding.
#[derive(Debug, Clone, Copy)]
pub struct Manual {
    /// A value only the user has is a `<placeholder>`, so it never reads as a command to
    /// run as is.
    pub instruction: &'static str,
    /// Why it is the user's to do.
    pub why: &'static str,
    pub remediation: fn(&Finding) -> Remediation,
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
    pub manual: Option<Manual>,
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
        // `-t` only for a target it resolves (a registered id), as the explain argv's.
        let with_target = |mut argv: Vec<String>| {
            if let Some(id) = registered_id(f) {
                argv.extend(["-t".to_owned(), id.to_owned()]);
            }
            argv
        };
        let base = match (self.manual, &self.fix) {
            (Some(manual), _) => (manual.remediation)(f),
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
        // `-t` only for a target it resolves, so the explain command runs.
        let mut explain_argv = argv_of(&["omaddons", "doctor", "explain", self.id]);
        if let Some(id) = registered_id(f) {
            explain_argv.extend(["-t".to_owned(), id.to_owned()]);
        }
        Remediation {
            command: base.command,
            argv: base.argv,
            explain_command: shell_words::join(&explain_argv),
            explain_argv,
            // An instance its detector marks as one recovery would not resume
            // (`evidence.resumable: false`), or as one its fixer cannot repair
            // (`evidence.fixable: false`), is no fixer's to repair now.
            auto_fixable: self.fix.is_some_and(|fix| fix.enabled && !fix.needs_force)
                && f.evidence.resumable != Some(false)
                && f.evidence.fixable != Some(false),
            needs_force: self.fix.is_some_and(|fix| fix.needs_force),
            estimated_actions: 0,
            destructive: base.destructive,
        }
    }
}

fn argv_of(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|part| (*part).to_owned()).collect()
}

/// The row of `id`, among the failure modes this process knows.
pub fn row(id: &str) -> Option<&'static FailureMode> {
    failure_modes().iter().find(|row| row.id == id)
}

/// The prefix of the test-only failure modes' ids (`doctor::test_fixers`): no catalog id
/// starts with it.
pub const TEST_ID_PREFIX: &str = "fm-test-";

/// Every failure mode this process knows: [`FAILURE_MODES`], and, in a debug build run
/// with `OMADDONS_TEST_FIXERS=1`, the test-only rows after them. Every runtime reader of
/// the registry goes through it; `FAILURE_MODES` stays the catalog the invariant and
/// coverage tests read. A release build never reads the variable.
pub fn failure_modes() -> &'static [FailureMode] {
    #[cfg(debug_assertions)]
    {
        static EFFECTIVE: std::sync::LazyLock<Vec<FailureMode>> = std::sync::LazyLock::new(|| {
            let on = std::env::var_os("OMADDONS_TEST_FIXERS").is_some_and(|v| v == "1");
            failure_modes_with(on).into_owned()
        });
        EFFECTIVE.as_slice()
    }
    #[cfg(not(debug_assertions))]
    {
        FAILURE_MODES
    }
}

/// [`FAILURE_MODES`], and the test-only rows after them when `test_fixers` (debug builds;
/// unit tests call it instead of setting the process's environment).
#[cfg(debug_assertions)]
pub fn failure_modes_with(test_fixers: bool) -> std::borrow::Cow<'static, [FailureMode]> {
    if test_fixers {
        std::borrow::Cow::Owned(
            FAILURE_MODES
                .iter()
                .chain(super::test_fixers::TEST_FAILURE_MODES)
                .copied()
                .collect(),
        )
    } else {
        std::borrow::Cow::Borrowed(FAILURE_MODES)
    }
}

// ---------------------------------------------------------------------------------------
// The rows.

/// What the rows over a client's full inventory read: the inventory needs the
/// discovered clients and their records, so a broken layer below it skips them with
/// its own cause.
const INVENTORY_LAYERS: &[Layer] = &[Layer::Discovery, Layer::Records, Layer::Inventory];

/// The declared cost of a row over a client's full inventory: the first such row pays
/// for the view building it, so each declares one full inventory of a realistic client.
const INVENTORY_COST_MS: u32 = 20;

/// A stuck row's `why`: its own opening, then what the set-aside does and when its undo
/// refuses, the same for rows 3-8.
macro_rules! set_aside_why {
    ($opening:literal) => {
        concat!(
            $opening,
            " Setting it aside is a forced repair: it moves the journal, its in-flight lock, its stage and both snapshot halves into quarantine, each on its own filesystem, and deletes nothing. The client is unblocked. Undo puts the whole set back exactly, but only while nothing it depends on has changed: after any install, update or other change omaddons makes on that client, a change to its marker, or (where the journal can be read) a change to a folder it names or to its WTF/ folder, undo refuses with undo_conflict and the set stays in quarantine, readable. Anything listed as not accounted for stays where it is and is not protected afterwards: omaddons' cleanup after a later change on that client may remove a leftover stage whose journal is gone, so copy what you need from it first. The set-aside is never part of a default fix: it needs this failure mode's id with --only, and --force and --yes."
        )
    };
}

/// Every failure mode, sorted by id.
pub static FAILURE_MODES: &[FailureMode] = &[
    FailureMode {
        id: ids::BLOB_HASH_MISMATCH,
        subsystem: Subsystem::Caches,
        severity: Severity::P2,
        title: "a cached download no longer matches its hash",
        description: "A file in omaddons' download cache is named by the SHA-256 of the archive it holds, and its bytes no longer hash to that name, or it cannot be read.",
        why: "omaddons never installs such a file: each use re-hashes it, so an install downloads it again when online and fails with hash_mismatch when offline. Quarantining it lets the next install download a good copy.",
        tier: Tier::Deep,
        online: false,
        // A full cache (BLOB_CACHE_MAX_BYTES, 1 GiB) at about 1 GB/s: half the deadline.
        cost_ms: 1_000,
        requires: &[Layer::Caches],
        detect: Some(caches::blob_hash_mismatch),
        fix: Some(FixerSpec {
            plan: super::fixers::quarantine::blob,
            writes_to: &[ScopeName::Cache],
            ops: &[FixKind::Quarantine],
            enabled: true,
            needs_force: false,
            cardinality: Cardinality::Low,
            preconditions: &[FixerPrecondition::LockHeld, FixerPrecondition::SameFs],
        }),
        manual: None,
    },
    FailureMode {
        id: ids::JOURNAL_MARKER_MISMATCH,
        subsystem: Subsystem::ConcurrencyPrimitives,
        severity: Severity::P0,
        title: "an interrupted change names a client directory that is not its client's",
        description: "The journal's client directory no longer carries its target's marker: the directory is gone, or it now belongs to another client. A doctor repair's journal that names a skill directory omaddons cannot attribute stops the same way.",
        why: set_aside_why!(
            "The journal names a client directory whose identity marker belongs to another client, so recovery never writes there, and every change on this client fails. A set-aside moves only what it finds by the transaction id on clients whose markers check out; whatever is in the mismatched directory stays where it is and is listed."
        ),
        tier: Tier::Cached,
        online: false,
        cost_ms: 1,
        requires: &[Layer::Journals],
        detect: Some(concurrency::journal_marker_mismatch),
        fix: Some(SET_ASIDE),
        manual: None,
    },
    FailureMode {
        id: ids::JOURNAL_PENDING,
        subsystem: Subsystem::ConcurrencyPrimitives,
        severity: Severity::P0,
        title: "an interrupted change waits to be finished",
        description: "A change's journal is still in the state directory, no omaddons process is writing it, and recovery can settle it: the next command that takes the lock rolls it back, rolls it forward or removes it. An interrupted doctor repair counts too.",
        why: "An interrupted change is waiting to be finished, and until then the client's folders may be half-swapped. The doctor's fix finishes it with omaddons' own recovery, the same one every change command runs first: it rolls the change back, or forward when its renames had all completed. The preview lists every recovery before it runs. This is not a doctor repair, so the doctor's undo does not revert it: a change that recovery completed is undone like any other change, by its transaction id; one it rolled back needs no undo. An interrupted settings restore is finished only while its game is closed.",
        tier: Tier::Cached,
        online: false,
        cost_ms: 1,
        requires: &[Layer::Journals],
        detect: Some(concurrency::journal_pending),
        fix: Some(DELEGATE),
        manual: None,
    },
    FailureMode {
        id: ids::JOURNAL_PRESERVATION_INCOMPLETE,
        subsystem: Subsystem::ConcurrencyPrimitives,
        severity: Severity::P1,
        title: "an interrupted restore has not finished keeping the settings it displaced",
        description: "Rolling back a WTF/ restore found settings played on since, or a staged copy whose backup is gone, and started keeping them as a recovered backup, but did not finish.",
        why: "Recovering an interrupted WTF/ restore could not finish giving the settings it displaced a home as a recovered backup, usually because the disk was full or an I/O error occurred. Everything is still in place under the transaction's stage. Recovery resumes with the same backup id once the cause is gone: free space or fix the error, then run the fix. When the evidence says resumption would stop for another reason, no fix is offered and the evidence names what blocks it.",
        tier: Tier::Cached,
        online: false,
        cost_ms: 1,
        requires: &[Layer::Journals],
        detect: Some(concurrency::journal_preservation_incomplete),
        fix: Some(DELEGATE),
        manual: Some(Manual {
            instruction: "free disk space if it is full or fix the I/O error; the doctor's fix then resumes keeping the settings, and the evidence says when something else blocks it",
            why: "recovery keeps everything until the backup is complete, and only the cause's removal lets it finish",
            remediation: preservation_manual,
        }),
    },
    FailureMode {
        id: ids::JOURNAL_STEP_UNRECOGNIZED,
        subsystem: Subsystem::ConcurrencyPrimitives,
        severity: Severity::P0,
        title: "an interrupted change stopped at a step it does not recognize",
        description: "Rolling back a change, or a doctor repair, found a recorded step whose paths match neither its before nor its after state.",
        why: set_aside_why!(
            "Rolling back an interrupted change met a step whose paths match neither its before nor its after state, so recovery stopped; the steps after it were already reversed. Every change on this client fails (for a repair transaction, every change that touches what it protects)."
        ),
        tier: Tier::Cached,
        online: false,
        cost_ms: 1,
        requires: &[Layer::Journals],
        detect: Some(concurrency::journal_step_unrecognized),
        fix: Some(SET_ASIDE),
        manual: None,
    },
    FailureMode {
        id: ids::JOURNAL_TOO_NEW,
        subsystem: Subsystem::ConcurrencyPrimitives,
        severity: Severity::P0,
        title: "an interrupted change was started by a newer omaddons",
        description: "The journal's format has a newer major version than this build reads.",
        why: set_aside_why!(
            "A newer omaddons started this change, and this build cannot read its journal, so every change on this client fails. The safer way is to finish it with that newer build. If you set it aside instead, this build moves only what it finds by the transaction id in omaddons' own directories, never a path the journal names, and says whether anything may remain unaccounted for."
        ),
        tier: Tier::Cached,
        online: false,
        cost_ms: 1,
        requires: &[Layer::Journals],
        detect: Some(concurrency::journal_too_new),
        fix: Some(SET_ASIDE),
        manual: None,
    },
    FailureMode {
        id: ids::JOURNAL_UNATTRIBUTED,
        subsystem: Subsystem::ConcurrencyPrimitives,
        severity: Severity::P0,
        title: "an interrupted change cannot be attributed to any client",
        description: "A journal names no target this build can read: its prepared line is missing, unreadable or newer, or the journal or its in-flight lock cannot be read.",
        why: set_aside_why!(
            "A journal whose client cannot be read blocks every change on every client. A set-aside moves the journal and everything its transaction id names in omaddons' own directories on clients whose markers check out."
        ),
        tier: Tier::Cached,
        online: false,
        cost_ms: 1,
        requires: &[Layer::Journals],
        detect: Some(concurrency::journal_unattributed),
        fix: Some(SET_ASIDE),
        manual: None,
    },
    FailureMode {
        id: ids::JOURNAL_UNEXPECTED_STATE,
        subsystem: Subsystem::ConcurrencyPrimitives,
        severity: Severity::P0,
        title: "an interrupted change to WTF/ stopped at a state it does not recognize",
        description: "Rolling back a WTF/ restore found a step whose paths match neither its before nor its after state, and could not place what it found.",
        why: set_aside_why!(
            "Recovering an interrupted WTF/ restore found something it did not record where a step left it, and stopped with everything exactly as found. Every change on this client fails until it is resolved. The set-aside, and its undo, need the game closed."
        ),
        tier: Tier::Cached,
        online: false,
        cost_ms: 1,
        requires: &[Layer::Journals],
        detect: Some(concurrency::journal_unexpected_state),
        fix: Some(SET_ASIDE),
        manual: None,
    },
    FailureMode {
        id: ids::JOURNAL_UNREADABLE,
        subsystem: Subsystem::ConcurrencyPrimitives,
        severity: Severity::P0,
        title: "an interrupted change's journal cannot be read",
        description: "A complete line of the journal does not decode, or no line is a readable prepared line.",
        why: set_aside_why!(
            "The journal holds lines this build cannot decode, so recovery cannot tell what ran, and every change on this client fails (on every client when its target cannot be read). A set-aside moves only what it finds by the transaction id in omaddons' own directories, never a path the journal names."
        ),
        tier: Tier::Cached,
        online: false,
        cost_ms: 1,
        requires: &[Layer::Journals],
        detect: Some(concurrency::journal_unreadable),
        fix: Some(SET_ASIDE),
        manual: None,
    },
    FailureMode {
        id: ids::JOURNAL_WAITS_FOR_GAME,
        subsystem: Subsystem::ConcurrencyPrimitives,
        severity: Severity::P2,
        title: "an interrupted change to WTF/ waits for the game to exit",
        description: "A restore of WTF/ (or the undo of one) was interrupted after it started moving WTF/, and the client is running or its running state cannot be told.",
        why: "Recovery never moves WTF/ while the game may write it, so every change on that client fails with game_running until the game exits; then the next change finishes or undoes it.",
        tier: Tier::Cached,
        online: false,
        cost_ms: 1,
        requires: &[Layer::Journals],
        detect: Some(concurrency::journal_waits_for_game),
        fix: None,
        manual: Some(Manual {
            instruction: "quit the game; the next change finishes or undoes it",
            why: "recovery never moves WTF/ while the game may write it",
            remediation: waits_manual,
        }),
    },
    FailureMode {
        id: ids::LOCK_HELD_LONG,
        subsystem: Subsystem::ConcurrencyPrimitives,
        severity: Severity::P2,
        title: "the lock has been held for a long time",
        // A new holder writes its record just after taking the lock, so for that instant
        // a reader may see the previous holder's start time; the next run is correct.
        description: "A live omaddons process has held the global lock for longer than LOCK_HELD_LONG (10 minutes); a commit normally holds it for milliseconds.",
        why: "Every change waits for the lock and gives up after 5 seconds with locked. The lock is released when its holder exits; omaddons never deletes or rewrites the lock file.",
        tier: Tier::Cached,
        online: false,
        cost_ms: 1,
        requires: &[Layer::Lock],
        detect: Some(concurrency::lock_held_long),
        fix: None,
        manual: Some(Manual {
            instruction: "if the process holding the lock is stuck, end it; the lock is released when it exits",
            why: "omaddons never touches the lock another process holds",
            remediation: lock_manual,
        }),
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
        manual: Some(Manual {
            instruction: "edit config.toml where the finding points: the line and column, the key, or the file's encoding, size or permissions",
            why: "config.toml is yours: omaddons never rewrites one it cannot read",
            remediation: view_failure_manual,
        }),
    },
    FailureMode {
        id: ids::CASE_COLLISION,
        subsystem: Subsystem::ExternalArtifacts,
        severity: Severity::P2,
        title: "folder names differ only in case",
        description: "AddOns holds two or more entries whose names differ only in case (Foo and foo). The client runs under Wine, which resolves names ignoring case, so it sees one folder under two names.",
        why: "Which copy the client loads is undefined, and every install, update or removal of that folder is refused with case_collision until one spelling is left.",
        tier: Tier::Default,
        online: false,
        cost_ms: INVENTORY_COST_MS,
        requires: INVENTORY_LAYERS,
        detect: Some(external::case_collision),
        fix: None,
        manual: Some(Manual {
            instruction: "keep one spelling of <folder> and move the others out of AddOns by hand, with the game closed",
            why: "omaddons never picks a spelling: either may hold your files",
            remediation: case_collision_manual,
        }),
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
        requires: &[Layer::Discovery, Layer::Records],
        detect: Some(external::managed_files_modified),
        fix: None,
        manual: Some(Manual {
            instruction: "keep the change, or put the release back with omaddons install <addon> -t <client> --reinstall",
            why: "only you know whether the change was intended, and a reinstall replaces it",
            remediation: reinstall_manual,
        }),
    },
    FailureMode {
        id: ids::OUT_OF_DATE_TOC,
        subsystem: Subsystem::ExternalArtifacts,
        severity: Severity::P3,
        title: "a managed addon loads only as out of date",
        description: "The TOC this client loads from a folder omaddons installed targets an older interface than the client's, so the client lists it as out of date.",
        why: "It loads only with \"Load out of date AddOns\" enabled, and an addon built for an older patch may misbehave.",
        tier: Tier::Default,
        online: false,
        cost_ms: INVENTORY_COST_MS,
        requires: INVENTORY_LAYERS,
        detect: Some(external::out_of_date_toc),
        fix: None,
        manual: Some(Manual {
            instruction: "update <addon>, or enable \"Load out of date AddOns\" in the client",
            why: "a newer build may not exist yet",
            remediation: update_manual,
        }),
    },
    FailureMode {
        id: ids::OWNERSHIP_CONFLICT,
        subsystem: Subsystem::ExternalArtifacts,
        severity: Severity::P1,
        title: "a folder is claimed by several addons",
        description: "Two or more addons in one client's record list the same AddOns folder (by its case-folded name). omaddons never records this itself: it comes from a hand-edited or damaged record.",
        why: "Removing any of those addons removes the folder for all of them, and an update of one replaces files the others rely on.",
        tier: Tier::Default,
        online: false,
        cost_ms: INVENTORY_COST_MS,
        requires: INVENTORY_LAYERS,
        detect: Some(external::ownership_conflict),
        fix: None,
        manual: Some(Manual {
            instruction: "decide which addon owns <folder>; reinstalling that addon with --force transfers the folder and marks the others degraded",
            why: "only you know which addon the folder belongs to",
            remediation: ownership_conflict_manual,
        }),
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
        manual: Some(Manual {
            instruction: "look at the degraded addons with omaddons list -t <client> -v, then reinstall or remove them",
            why: "which addon should own the folder is your choice",
            remediation: inspect_manual,
        }),
    },
    FailureMode {
        id: ids::WRONG_FLAVOR,
        subsystem: Subsystem::ExternalArtifacts,
        severity: Severity::P2,
        title: "a managed addon's folder no longer loads on this client",
        description: "The TOC this client loads from a folder omaddons installed no longer passes the client's load rules: its interface is outside the client's range or below its floor, a game-type rule excludes the client, or the folder has no TOC the client loads. Usually the client was patched, or the TOC was edited.",
        why: "The client skips the folder, so the addon, or that part of it, does nothing. A newer build may load; if none exists, the addon is not built for this client and removing it is the other way out.",
        tier: Tier::Default,
        online: false,
        cost_ms: INVENTORY_COST_MS,
        requires: INVENTORY_LAYERS,
        detect: Some(external::wrong_flavor),
        fix: None,
        manual: Some(Manual {
            instruction: "update <addon>; if no build for this client exists, remove it",
            why: "whether a build for this client exists is the publisher's decision",
            remediation: update_manual,
        }),
    },
    FailureMode {
        id: ids::NETWORK_CREDENTIAL_REJECTED,
        subsystem: Subsystem::Network,
        severity: Severity::P1,
        title: "GitHub rejects the configured token",
        description: "GitHub answered 401 to the token omaddons sends (from OMADDONS_GITHUB_TOKEN, config.toml's [sources.github] credential or the stored token file), or the configured credential cannot be read at all.",
        why: "Every REST request fails with auth_rejected or auth_required until the token is replaced or removed; the main GitHub path needs no token and keeps working.",
        tier: Tier::Default,
        online: true,
        // One request to /rate_limit.
        cost_ms: 500,
        requires: &[Layer::Config],
        detect: Some(network::credential_rejected),
        fix: None,
        manual: Some(Manual {
            instruction: "replace or remove the token where it comes from: clear a stored token with auth github clear, unset or replace OMADDONS_GITHUB_TOKEN, or correct [sources.github] credential in config.toml",
            why: "only you can issue a new token, and which credential to use is yours to decide",
            remediation: credential_manual,
        }),
    },
    FailureMode {
        id: ids::NETWORK_GITHUB_RATE_LIMITED,
        subsystem: Subsystem::Network,
        severity: Severity::P2,
        title: "GitHub's REST API budget is spent",
        description: "GitHub's /rate_limit answer says 5 or fewer core REST requests remain this hour for the token omaddons sends, or for this address without a token.",
        why: "omaddons stops its REST requests at that reserve, so the beta and alpha channels, repositories without a release.json and search fail with rate_limited until the reset; a stable install needs no REST and keeps working.",
        tier: Tier::Default,
        online: true,
        // One request to /rate_limit, which costs none of the budget.
        cost_ms: 500,
        requires: &[Layer::Config],
        detect: Some(network::github_rate_limited),
        fix: None,
        manual: Some(Manual {
            instruction: "wait until the reset time the finding gives, or pipe a token to omaddons auth github set for 5,000 requests an hour",
            why: "only time resets GitHub's budget, and a token is an account's to give",
            remediation: rate_limited_manual,
        }),
    },
    FailureMode {
        id: ids::NETWORK_SOURCE_UNREACHABLE,
        subsystem: Subsystem::Network,
        severity: Severity::P2,
        title: "an addon source does not answer",
        description: "One cheap request to an enabled source that an installed addon comes from (GitHub, WoWInterface, Tukui or WowAce) got no HTTP answer, a server error or a 429.",
        why: "Installs, updates and checks from that source fail until it answers again. The doctor sends one request per source and does not retry it.",
        tier: Tier::Default,
        online: true,
        // One request per source, up to four.
        cost_ms: 1_500,
        requires: &[Layer::Config, Layer::Records],
        detect: Some(network::source_unreachable),
        fix: None,
        manual: Some(Manual {
            instruction: "check this machine's network connection and the source's host, then run the online checks again later",
            why: "omaddons cannot make a source answer, and an outage usually passes",
            remediation: unreachable_manual,
        }),
    },
    FailureMode {
        id: ids::SKILL_VERSION_DRIFT,
        subsystem: Subsystem::Plugins,
        severity: Severity::P3,
        title: "an installed agent skill holds another version of the guide",
        description: "A SKILL.md omaddons installed for coding agents (in the default skill folder, or in a folder robot-docs install-skill wrote) carries a guide stamp other than this build's guide, or no longer matches its own stamp.",
        why: "Coding agents load SKILL.md to learn omaddons, so an outdated guide teaches commands, codes and rules this build may not have. The check compares the stamp on the line after the file's frontmatter with this build's guide digest and never renders the guide. The doctor never rewrites a skill: reinstall it, and install-skill keeps a copy you edited as SKILL.md.bak.",
        tier: Tier::Cached,
        online: false,
        // At most SKILL_DIRS_MAX + 1 bounded single-file reads, no walk.
        cost_ms: 1,
        requires: &[Layer::Skill],
        detect: Some(plugins::skill_version_drift),
        fix: None,
        manual: Some(Manual {
            instruction: "reinstall the skill from this build with robot-docs install-skill, with --dir <dir> for a folder other than the default",
            why: "the skill is yours: the doctor never rewrites it, and install-skill keeps an edited copy",
            remediation: skill_manual,
        }),
    },
    FailureMode {
        id: ids::WIDGET_INVALID,
        subsystem: Subsystem::Plugins,
        severity: Severity::P2,
        title: "the installed bar widget fails Omarchy's plugin validation",
        description: "The bar widget's folder under Omarchy's plugin directory breaks one or more rules of Omarchy's plugin validator (its manifest, its entry points, or a symlink inside it), as omaddons' port of that validator checks them.",
        why: "Omarchy's shell may refuse to load a plugin that fails validation, so the bar loses the widget. A reinstall renders a valid one; the doctor never runs it itself, because it runs Omarchy's tools in the desktop session and cannot be undone.",
        tier: Tier::Quick,
        online: false,
        // One bounded manifest read and a walk of the widget's few dozen entries.
        cost_ms: 2,
        requires: &[Layer::Widget],
        detect: Some(plugins::widget_invalid),
        fix: None,
        manual: Some(Manual {
            instruction: "reinstall the bar widget from this build with its omarchy install, from the desktop session; omaddons omarchy status lists each problem with its message",
            why: "the widget lives in your desktop's configuration, which only the omarchy commands change",
            remediation: widget_invalid_manual,
        }),
    },
    FailureMode {
        id: ids::WIDGET_VERSION_DRIFT,
        subsystem: Subsystem::Plugins,
        severity: Severity::P3,
        title: "the bar widget is from another omaddons build",
        description: "The installed bar widget's code is not what this binary renders for the directories it was installed under.",
        why: "An outdated widget stays read-only and may misread this build's output. The doctor reports it and never reinstalls it: a reinstall runs Omarchy's own tools in the desktop session and cannot be undone.",
        tier: Tier::Cached,
        online: false,
        cost_ms: 1,
        requires: &[Layer::Widget],
        detect: Some(plugins::widget_version_drift),
        fix: None,
        manual: Some(Manual {
            instruction: "reinstall the bar widget from this build with its omarchy install",
            why: "the widget lives in your desktop's configuration, which only the omarchy commands change",
            remediation: widget_manual,
        }),
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
        manual: Some(Manual {
            instruction: "restore the file the finding names from a backup of the state directory, or repair what the finding says is wrong with it",
            why: "omaddons never rewrites a state file it cannot read: what it held may still matter",
            remediation: view_failure_manual,
        }),
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
        manual: Some(Manual {
            instruction: "upgrade omaddons to the build that wrote the file, or a newer one",
            why: "this build cannot know what a newer one recorded, so it never rewrites the file",
            remediation: upgrade_manual,
        }),
    },
    FailureMode {
        id: ids::CREDENTIAL_FILE_MODE,
        subsystem: Subsystem::Secrets,
        severity: Severity::P2,
        title: "a credential file is readable by other users",
        description: "A file holding a credential (the stored GitHub token, another key file in omaddons' credentials directory, or a file a [sources] credential in config.toml names) grants its group or others any permission.",
        why: "Anyone else who can log in to this machine, and any process running as another user, can read the secret and use it. The repair sets mode 0600 when the file lies inside omaddons' config directory; undoing that repair restores the old mode, and with it the exposure. A file elsewhere is reported with the chmod to run.",
        tier: Tier::Cached,
        online: false,
        cost_ms: 1,
        requires: &[Layer::Credentials],
        detect: Some(secrets::credential_file_mode),
        fix: Some(super::fixers::chmod::SPEC),
        manual: Some(Manual {
            instruction: "set the file to mode 0600; a symlink, a file outside omaddons' config directory or another user's file is yours to change: chmod 600 <file>",
            why: "the doctor writes only files inside omaddons' own config directory, as their owner",
            remediation: credential_mode_manual,
        }),
    },
    FailureMode {
        id: ids::CHECK_INCOMPLETE,
        subsystem: Subsystem::StateFiles,
        severity: Severity::P3,
        title: "a check could not finish within its limit",
        description: "A detector stopped at its deadline or its entry limit during this run, or an online check's request got no answer it could judge, so what it checks was only partly checked.",
        why: "Incomplete coverage must never read as a pass.",
        tier: Tier::Runtime,
        online: false,
        cost_ms: 0,
        requires: &[],
        detect: None,
        fix: None,
        manual: Some(Manual {
            instruction: "run the check alone with a larger budget, omaddons doctor --only <fm-id> --budget <ms>, or narrow it with -t <client>",
            why: "a check its limit stopped looked at only part of what it covers",
            remediation: incomplete_manual,
        }),
    },
    FailureMode {
        id: ids::DETECTOR_CRASHED,
        subsystem: Subsystem::StateFiles,
        severity: Severity::P1,
        title: "a check stopped with an internal error",
        description: "A detector panicked during this run; the run went on without its result.",
        why: "A check that did not run cannot say the state is healthy.",
        tier: Tier::Runtime,
        online: false,
        cost_ms: 0,
        requires: &[],
        detect: None,
        fix: None,
        manual: Some(Manual {
            instruction: "report the check and the location in omaddons' source that the finding names",
            why: "a check that panics is a bug in omaddons, which no command repairs",
            remediation: crashed_manual,
        }),
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
        requires: &[Layer::Discovery, Layer::Records],
        detect: Some(state_files::record_folder_missing),
        fix: None,
        manual: Some(Manual {
            instruction: "put the addon back with omaddons install <addon> -t <client> --reinstall, or remove it with omaddons remove <addon> -t <client>",
            why: "only you know whether the folder was removed on purpose",
            remediation: reinstall_manual,
        }),
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
        requires: &[Layer::Discovery],
        detect: Some(userland::target_vanished),
        fix: None,
        manual: Some(Manual {
            instruction: "if the client is gone for good, forget it with omaddons targets rm <client>",
            why: "only you know whether the client is gone for good, or only moved or unmounted",
            remediation: forget_manual,
        }),
    },
];

/// The delegation (M4-D37) of `journal-pending` and `journal-preservation-incomplete`:
/// `doctor fix` has omaddons' own recovery settle the journals its preview disclosed. The
/// scopes are those recovery writes as the doctor sees it; a restore's or an undo's
/// recovery also writes `WTF/` and SavedVariables, under the engine's own writers
/// (`BackupRestore`, `Undo`) and rules, never as `Writer::Doctor`.
const DELEGATE: FixerSpec = FixerSpec {
    plan: super::fixers::recover::plan,
    writes_to: &[ScopeName::State, ScopeName::ClientMeta, ScopeName::AddOns],
    ops: &[FixKind::Delegate],
    enabled: true,
    needs_force: false,
    cardinality: Cardinality::Low,
    preconditions: &[FixerPrecondition::LockHeld],
};

/// The set-aside (M4-D38) of the six stuck journal states (rows 3-8): it suspends the
/// journal's set into quarantine on each filesystem, only with its id in `--only` and
/// `--force --yes` (M4-D39: never part of a default fix).
const SET_ASIDE: FixerSpec = FixerSpec {
    plan: super::fixers::set_aside::plan,
    writes_to: &[ScopeName::State, ScopeName::ClientMeta],
    ops: &[FixKind::Suspend],
    enabled: false,
    needs_force: true,
    cardinality: Cardinality::Low,
    preconditions: &[
        FixerPrecondition::LockHeld,
        FixerPrecondition::InflightFree,
        FixerPrecondition::ClientNotRunning,
        FixerPrecondition::SameFs,
    ],
};

/// `journal-preservation-incomplete`, by instance: the fix while recovery would resume it
/// (`evidence.resumable`), else its evidence, which says what blocks it.
fn preservation_manual(f: &Finding) -> Remediation {
    if f.evidence.resumable != Some(true) {
        return explain_manual(f);
    }
    // `-t` as every fixer remediation names it (`FailureMode::remediation`, E6's
    // `remediation_for`), so `status` and `doctor` print the same command.
    let mut argv = argv_of(&["omaddons", "doctor", "fix", "--only", f.id]);
    if let Some(id) = registered_id(f) {
        argv.extend(["-t".to_owned(), id.to_owned()]);
    }
    Remediation {
        command: shell_words::join(&argv),
        argv: Some(argv),
        ..Remediation::default()
    }
}

/// A settings restore waiting for its game: recovery's own advice.
fn waits_manual(_: &Finding) -> Remediation {
    prose("quit the game; the next change finishes or undoes it")
}

/// A stuck journal, until its fixer exists: the evidence, which runs.
fn explain_manual(f: &Finding) -> Remediation {
    let mut argv = argv_of(&["omaddons", "doctor", "explain", f.id]);
    if let Some(id) = registered_id(f) {
        argv.extend(["-t".to_owned(), id.to_owned()]);
    }
    Remediation {
        command: shell_words::join(&argv),
        argv: Some(argv),
        ..Remediation::default()
    }
}

/// A lock held long: advice naming its holder; omaddons never touches the lock file.
fn lock_manual(f: &Finding) -> Remediation {
    let pid = f
        .evidence
        .pid
        .map_or_else(|| "<pid>".to_owned(), |pid| pid.to_string());
    prose(&format!(
        "if the process holding the lock (pid {pid}) is stuck, end it (kill {pid}); the lock is released when it exits, and omaddons never touches the lock file"
    ))
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

/// An installed skill: its reinstall from this build (`--dir` for a folder other than the
/// default), which keeps an edited copy as `SKILL.md.bak`. A symlinked `SKILL.md`, which
/// install-skill never writes through, and a file it cannot replace (not a regular file,
/// or unreadable) get prose.
fn skill_manual(f: &Finding) -> Remediation {
    let Some(file) = f.evidence.path.as_deref() else {
        return prose("reinstall the agent skill from this build with robot-docs install-skill");
    };
    let dir = file.parent().unwrap_or(file);
    match f.evidence.reason {
        Some(plugins::SKILL_SYMLINK) => {
            return prose(&format!(
                "{} is a symbolic link, which install-skill never writes through: update the file it points to with this build's guide, or replace the link with a regular file and reinstall the skill",
                file.display()
            ));
        }
        Some(plugins::SKILL_NOT_A_FILE) => {
            return prose(&format!(
                "{} is not a regular file: move it out of the skill folder, then reinstall the skill",
                file.display()
            ));
        }
        Some(plugins::SKILL_UNREADABLE) => {
            return prose(&format!(
                "{} cannot be read: check its permissions, then reinstall the skill",
                file.display()
            ));
        }
        _ => {}
    }
    let mut argv: Vec<String> = ["omaddons", "robot-docs", "install-skill"]
        .map(str::to_owned)
        .to_vec();
    let default = crate::state::paths::Paths::from_env()
        .ok()
        .map(|paths| paths.skill_dir());
    let resolved =
        |path: &std::path::Path| std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    if default.as_deref().map(resolved) != Some(resolved(dir)) {
        argv.push("--dir".to_owned());
        argv.push(dir.display().to_string());
    }
    hint(&CommandHint::new(
        crate::contract::actions::Action::InstallSkill,
        argv,
    ))
}

/// An invalid widget: its reinstall, unless it serves another omaddons state, which a
/// reinstall from this run would point at this run's state instead (M3-D7): prose then.
fn widget_invalid_manual(f: &Finding) -> Remediation {
    if f.evidence.reason == Some(plugins::OTHER_CONTEXT) {
        return prose(
            "reinstall the widget with the omaddons binary and the HOME and XDG directories it was installed under",
        );
    }
    widget_manual(f)
}

/// Which addon owns a folder is the user's choice: prose naming the claimants.
fn ownership_conflict_manual(f: &Finding) -> Remediation {
    let owners = f.evidence.owners.as_deref().unwrap_or_default().join(", ");
    let folder = f.folder.as_deref().unwrap_or("<folder>");
    prose(&format!(
        "{owners} on {} all record {folder}: decide which addon owns it; reinstalling that addon with --force transfers the folder and marks the others degraded, and removing any of them removes the folder for all",
        target_id(f)
    ))
}

/// Which spelling to keep is the user's choice: prose naming the spellings.
fn case_collision_manual(f: &Finding) -> Remediation {
    let paths = f.evidence.paths.as_deref().unwrap_or_default();
    let names: Vec<String> = paths
        .iter()
        .filter_map(|path| path.file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .collect();
    let dir = paths
        .first()
        .and_then(|path| path.parent())
        .map_or_else(|| "AddOns".to_owned(), |dir| dir.display().to_string());
    prose(&format!(
        "keep one of {} in {dir} and move the others out by hand, with the game closed; omaddons refuses to change any of them until one spelling is left",
        names.join(", ")
    ))
}

/// A newer build may load, or load as current: the addon's update on its client.
fn update_manual(f: &Finding) -> Remediation {
    let argv = argv_of(&[
        "omaddons",
        "update",
        f.addon.as_deref().unwrap_or_default(),
        "-t",
        target_id(f),
    ]);
    Remediation {
        command: shell_words::join(&argv),
        argv: Some(argv),
        ..Remediation::default()
    }
}

/// A crashed check is a bug: no command helps.
fn crashed_manual(_: &Finding) -> Remediation {
    prose("this is a bug in omaddons: report the detector and location above")
}

/// A token GitHub refuses, or that cannot be had: undone where it comes from, as
/// `GithubToken::rejected` says it. A stored one is cleared (not destructive: REST then
/// runs without it); the environment and config.toml are the user's to edit.
fn credential_manual(f: &Finding) -> Remediation {
    match f.evidence.origin {
        Some(SecretOrigin::Env) => prose(&format!(
            "replace or unset {}",
            crate::sources::github_rest::TOKEN_VAR
        )),
        Some(SecretOrigin::Config) => prose("correct [sources.github] credential in config.toml"),
        Some(SecretOrigin::File) | None => {
            let argv = argv_of(&["omaddons", "auth", "github", "clear"]);
            Remediation {
                command: shell_words::join(&argv),
                argv: Some(argv),
                ..Remediation::default()
            }
        }
    }
}

/// An exposed credential: the chmod's `doctor fix` when the doctor can repair it (no `-t`:
/// a machine finding), else the chmod to run, by why the doctor cannot.
fn credential_mode_manual(f: &Finding) -> Remediation {
    if f.evidence.fixable != Some(false) {
        let argv = argv_of(&["omaddons", "doctor", "fix", "--only", f.id]);
        return Remediation {
            command: shell_words::join(&argv),
            argv: Some(argv),
            ..Remediation::default()
        };
    }
    let path = f
        .evidence
        .path
        .as_deref()
        .map_or_else(|| "<file>".to_owned(), |path| path.display().to_string());
    prose(&match f.evidence.reason {
        Some("symlink") => format!("{path} is a symlink: run chmod 600 on the file it points to"),
        Some("other_owner") => {
            format!("{path} belongs to another user: run chmod 600 on it as that user")
        }
        _ => format!("chmod 600 {path}"),
    })
}

/// A spent REST budget: wait for its reset, or, without a token, store one. A token goes
/// on standard input, never argv, so this stays prose.
fn rate_limited_manual(f: &Finding) -> Remediation {
    let wait = f.evidence.rate_limit.map_or_else(
        || "wait for GitHub's hourly reset".to_owned(),
        |r| format!("wait until {}", r.reset_at.strftime("%Y-%m-%d %H:%M UTC")),
    );
    if f.evidence.authenticated == Some(true) {
        prose(&wait)
    } else {
        prose(&format!(
            "{wait}, or pipe a token to omaddons auth github set for 5,000 requests an hour"
        ))
    }
}

/// A source that does not answer: advice naming its host.
fn unreachable_manual(f: &Finding) -> Remediation {
    let host = f.evidence.host.as_deref().unwrap_or("the source's host");
    prose(&format!(
        "check this machine's network connection and {host}, then run the online checks again later"
    ))
}

/// A check past its deadline runs again alone with ten times as long (a read); an online
/// check that got no answer it could judge asks again; one past its entry cap can only be
/// narrowed.
fn incomplete_manual(f: &Finding) -> Remediation {
    if let (Some(fm), Some("unanswered")) = (f.evidence.detector, f.evidence.reason) {
        let argv = argv_of(&["omaddons", "doctor", "--online", "--only", fm]);
        return Remediation {
            command: shell_words::join(&argv),
            argv: Some(argv),
            ..Remediation::default()
        };
    }
    match (f.evidence.detector, f.evidence.deadline_ms) {
        (Some(fm), Some(ms)) => {
            let budget = format!("{}ms", ms.saturating_mul(10));
            let argv = argv_of(&["omaddons", "doctor", "--only", fm, "--budget", &budget]);
            Remediation {
                command: shell_words::join(&argv),
                argv: Some(argv),
                ..Remediation::default()
            }
        }
        _ => prose(&format!(
            "this check visits at most {} entries; narrow it with -t <client>",
            thousands(f.evidence.max_entries.unwrap_or(DOCTOR_SCAN_MAX_ENTRIES))
        )),
    }
}

/// `n` with a comma between each group of three digits.
fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (at, digit) in digits.chars().enumerate() {
        if at > 0 && (digits.len() - at).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// The target id of a target's finding (always set on the rows that read it).
fn target_id(f: &Finding) -> &str {
    f.target.as_ref().map_or("", |target| target.id.as_str())
}

/// The finding's target id when `-t` resolves it: a registered target's own id, never
/// the key string an unregistered one reads as (`DoctorView::target_ref`).
fn registered_id(f: &Finding) -> Option<&str> {
    f.target
        .as_ref()
        .filter(|target| target.id != target.key.to_string())
        .map(|target| target.id.as_str())
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
    /// A row of a detecting tier with no detector.
    NoDetector,
    /// A `runtime` row with a detector: the runtime raises it, nothing detects it.
    RuntimeRowWithDetector,
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
/// the fast tiers; a detector exactly on the rows that are not `runtime`'s; a fixer only
/// with a detector, ops and a scope fsx lets the doctor write.
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
        // A test row's prefix is its own marker, and test rows follow the catalog in
        // their declared order: neither rule binds them.
        let test_row = row.id.starts_with(TEST_ID_PREFIX);
        if !test_row {
            if previous.is_some_and(|previous| previous > row.id) {
                broken(Rule::NotSorted);
            }
            previous = Some(row.id);
        }
        if !is_fm_id(row.id) {
            broken(Rule::BadId);
        } else if !test_row && subsystem_part(row.id) != Some(row.subsystem.as_str()) {
            broken(Rule::SubsystemMismatch);
        }
        if !row.subsystem.applicable() {
            broken(Rule::InapplicableSubsystem);
        }
        let well_formed =
            |text: &str| !text.is_empty() && text.trim() == text && !text.contains('\n');
        let manual_texts = row
            .manual
            .iter()
            .flat_map(|manual| [manual.instruction, manual.why]);
        if ![row.title, row.description, row.why]
            .into_iter()
            .chain(manual_texts)
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
        match (row.tier, row.detect.is_some()) {
            (Tier::Runtime, true) => broken(Rule::RuntimeRowWithDetector),
            (Tier::Runtime, false) | (_, true) => {}
            (_, false) => broken(Rule::NoDetector),
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
            manual: Some(Manual {
                instruction: "do the thing by hand",
                why: "only you can",
                remediation: manual,
            }),
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
                "empty_manual_instruction",
                vec![with(|r| {
                    r.manual = r.manual.map(|m| Manual {
                        instruction: "",
                        ..m
                    });
                })],
                vec![Rule::EmptyText],
            ),
            (
                "untrimmed_manual_why",
                vec![with(|r| {
                    r.manual = r.manual.map(|m| Manual {
                        why: "only you can ",
                        ..m
                    });
                })],
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
                "no_detector",
                vec![with(|r| r.detect = None)],
                vec![Rule::NoDetector],
            ),
            (
                "runtime_row_without_detector",
                vec![with(|r| {
                    r.tier = Tier::Runtime;
                    r.detect = None;
                })],
                vec![],
            ),
            (
                "runtime_row_with_detector",
                vec![with(|r| r.tier = Tier::Runtime)],
                vec![Rule::RuntimeRowWithDetector],
            ),
            (
                "fixer_without_detector",
                vec![with(|r| {
                    r.detect = None;
                    r.fix = Some(fixer());
                })],
                vec![Rule::NoDetector, Rule::FixerWithoutDetector],
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
            // Only `journal-pending` has a fixer among them: the delegation (F2).
            let report_only = id != ids::JOURNAL_PENDING;
            assert_eq!(
                row.map(|r| (r.severity, r.tier, r.online, r.fix.is_none())),
                Some((severity, tier, false, report_only)),
                "{id}"
            );
        }
        // The seven, the two the doctor's view raises, the two its runtime raises, the four
        // inventory rows of `external_artifacts`, the nine journal and lock rows of
        // `concurrency_primitives`, `caches`' blob row (F6), and F7's three online rows of
        // `network`, the `secrets` row, `plugins`' widget-invalid row and its
        // skill-version-drift row (M4-D64).
        assert_eq!(FAILURE_MODES.len(), 31);
    }

    /// Appendix B rows 1-10: the journal and lock rows, all `cached` and report-only.
    #[test]
    fn the_ten_concurrency_rows_match_appendix_b() {
        for (id, severity, requires) in [
            (ids::JOURNAL_PENDING, Severity::P0, Layer::Journals),
            (ids::JOURNAL_WAITS_FOR_GAME, Severity::P2, Layer::Journals),
            (ids::JOURNAL_TOO_NEW, Severity::P0, Layer::Journals),
            (ids::JOURNAL_UNREADABLE, Severity::P0, Layer::Journals),
            (ids::JOURNAL_UNEXPECTED_STATE, Severity::P0, Layer::Journals),
            (
                ids::JOURNAL_STEP_UNRECOGNIZED,
                Severity::P0,
                Layer::Journals,
            ),
            (ids::JOURNAL_MARKER_MISMATCH, Severity::P0, Layer::Journals),
            (ids::JOURNAL_UNATTRIBUTED, Severity::P0, Layer::Journals),
            (
                ids::JOURNAL_PRESERVATION_INCOMPLETE,
                Severity::P1,
                Layer::Journals,
            ),
            (ids::LOCK_HELD_LONG, Severity::P2, Layer::Lock),
        ] {
            let row = row(id).unwrap();
            assert_eq!(row.subsystem, Subsystem::ConcurrencyPrimitives, "{id}");
            assert_eq!(row.severity, severity, "{id}");
            assert_eq!(row.tier, Tier::Cached, "{id}");
            assert_eq!(row.requires, &[requires], "{id}");
            assert!(!row.online && row.cost_ms == 1, "{id}");
            // Rows 1 and 9 delegate, rows 3-8 set aside (F2); rows 2 and 10 are advice.
            let advice = [ids::JOURNAL_WAITS_FOR_GAME, ids::LOCK_HELD_LONG];
            assert!(row.detect.is_some(), "{id}");
            assert_eq!(row.fix.is_none(), advice.contains(&id), "{id}");
            let manual = row.manual.iter().flat_map(|m| [m.instruction, m.why]);
            assert!(
                row.manual.is_some() || row.fix.is_some(),
                "{id}: a remediation"
            );
            for text in [row.title, row.description, row.why]
                .into_iter()
                .chain(manual)
            {
                assert!(!text.is_empty() && !text.contains('\n'), "{id}: {text}");
                let words: Vec<&str> = text.split_whitespace().collect();
                assert!(
                    !words.windows(2).any(|pair| pair[0] == "omaddons"
                        && crate::app::COMMANDS
                            .iter()
                            .any(|c| c.name.split_whitespace().next() == Some(pair[1]))),
                    "{id}: names a runnable command: {text}"
                );
            }
        }
        assert!(invariant_violations(FAILURE_MODES).is_empty());
    }

    /// The six stuck rows (Appendix B rows 3-8).
    const STUCK: [&str; 6] = [
        ids::JOURNAL_TOO_NEW,
        ids::JOURNAL_UNREADABLE,
        ids::JOURNAL_UNEXPECTED_STATE,
        ids::JOURNAL_STEP_UNRECOGNIZED,
        ids::JOURNAL_MARKER_MISMATCH,
        ids::JOURNAL_UNATTRIBUTED,
    ];

    /// Rows 3-8 carry the shared set-aside (M4-D38), disabled and forced; rows 1 and 9
    /// do not.
    #[test]
    fn rows_3_to_8_carry_the_set_aside_fixer() {
        for id in STUCK {
            let fix = row(id).unwrap().fix.unwrap();
            assert_eq!(fix.ops, [FixKind::Suspend], "{id}");
            assert!(!fix.enabled && fix.needs_force, "{id}");
            assert_eq!(
                fix.writes_to,
                [ScopeName::State, ScopeName::ClientMeta],
                "{id}"
            );
            assert_eq!(
                fix.preconditions,
                [
                    FixerPrecondition::LockHeld,
                    FixerPrecondition::InflightFree,
                    FixerPrecondition::ClientNotRunning,
                    FixerPrecondition::SameFs,
                ],
                "{id}"
            );
        }
        for id in [ids::JOURNAL_PENDING, ids::JOURNAL_PRESERVATION_INCOMPLETE] {
            assert_ne!(
                row(id).unwrap().fix.unwrap().ops,
                [FixKind::Suspend],
                "{id}"
            );
        }
        assert!(invariant_violations(FAILURE_MODES).is_empty());
    }

    /// The texts read at the worst moment say what the set-aside does and when its undo
    /// refuses, in plain sentences that name no runnable command.
    #[test]
    fn why_texts_say_what_they_must() {
        for id in STUCK {
            let why = row(id).unwrap().why;
            for must in ["quarantine", "deletes nothing", "undo refuses", "readable"] {
                assert!(why.contains(must), "{id}: {must:?} in {why}");
            }
            assert!(!why.contains('!'), "{id}");
            let words: Vec<&str> = why.split_whitespace().collect();
            assert!(
                !words.windows(2).any(|pair| pair[0] == "omaddons"
                    && crate::app::COMMANDS
                        .iter()
                        .any(|c| c.name.split_whitespace().next() == Some(pair[1]))),
                "{id}: names a runnable command: {why}"
            );
        }
        assert!(
            row(ids::JOURNAL_TOO_NEW)
                .unwrap()
                .why
                .contains("newer build")
        );
        for id in [ids::JOURNAL_TOO_NEW, ids::JOURNAL_UNREADABLE] {
            assert!(
                row(id)
                    .unwrap()
                    .why
                    .contains("never a path the journal names"),
                "{id}"
            );
        }
    }

    /// Rows 1 and 9 carry the delegation (M4-D37): enabled, no force, one op, and row 9's
    /// remediation is its fix only while its detector says recovery would resume it.
    #[test]
    fn rows_1_and_9_carry_the_delegate_fixer() {
        for id in [ids::JOURNAL_PENDING, ids::JOURNAL_PRESERVATION_INCOMPLETE] {
            let fix = row(id).unwrap().fix.unwrap();
            assert_eq!(fix.ops, [FixKind::Delegate], "{id}");
            assert!(fix.enabled && !fix.needs_force, "{id}");
            assert_eq!(fix.cardinality, Cardinality::Low, "{id}");
            assert_eq!(fix.preconditions, [FixerPrecondition::LockHeld], "{id}");
            assert_eq!(
                fix.writes_to,
                [ScopeName::State, ScopeName::ClientMeta, ScopeName::AddOns],
                "{id}"
            );
        }
        let row9 = row(ids::JOURNAL_PRESERVATION_INCOMPLETE).unwrap();
        let instance = |resumable: Option<bool>| {
            let mut f = finding(row9, true);
            f.evidence.resumable = resumable;
            row9.remediation(&f)
        };
        let resumable = instance(Some(true));
        assert_eq!(
            resumable.argv,
            Some(argv_of(&[
                "omaddons",
                "doctor",
                "fix",
                "--only",
                ids::JOURNAL_PRESERVATION_INCOMPLETE,
                "-t",
                "retail"
            ])),
            "{resumable:?}"
        );
        assert!(resumable.auto_fixable, "{resumable:?}");
        let stops = instance(Some(false));
        assert_eq!(stops.argv.as_ref(), Some(&stops.explain_argv), "{stops:?}");
        assert!(!stops.auto_fixable, "{stops:?}");
        assert!(invariant_violations(FAILURE_MODES).is_empty());
    }

    #[test]
    fn for_stop_reason_is_total() {
        use crate::engine::journal::StopReason;
        for reason in StopReason::ALL {
            let id = ids::for_stop_reason(reason);
            let row = row(id).unwrap();
            assert_eq!(
                row.subsystem,
                Subsystem::ConcurrencyPrimitives,
                "{reason:?}"
            );
            assert_ne!(
                id,
                ids::JOURNAL_PENDING,
                "{reason:?}: a stop is never pending"
            );
        }
    }

    /// Appendix B rows 30-33: report-only rows over a client's record and full inventory.
    #[test]
    fn the_inventory_rows_match_appendix_b() {
        for (id, severity) in [
            (ids::OWNERSHIP_CONFLICT, Severity::P1),
            (ids::CASE_COLLISION, Severity::P2),
            (ids::WRONG_FLAVOR, Severity::P2),
            (ids::OUT_OF_DATE_TOC, Severity::P3),
        ] {
            let row = row(id).unwrap();
            assert_eq!(row.subsystem, Subsystem::ExternalArtifacts, "{id}");
            assert_eq!(row.severity, severity, "{id}");
            assert_eq!(row.tier, Tier::Default, "{id}");
            assert_eq!(
                row.requires,
                &[Layer::Discovery, Layer::Records, Layer::Inventory],
                "{id}"
            );
            assert!(!row.online && row.cost_ms == 20, "{id}");
            assert!(row.detect.is_some() && row.fix.is_none(), "{id}");
            for text in [row.title, row.description, row.why] {
                assert!(!text.is_empty(), "{id}");
            }
            let manual = row.manual.unwrap();
            assert!(
                !manual.instruction.is_empty() && !manual.why.is_empty(),
                "{id}"
            );
            let words: Vec<&str> = manual.instruction.split_whitespace().collect();
            assert!(
                !words.windows(2).any(|pair| pair[0] == "omaddons"),
                "{id}: the manual pair names no runnable command"
            );
        }
        assert!(invariant_violations(FAILURE_MODES).is_empty());
    }

    /// Appendix B rows 42-44: the online rows of `network`, `default` (invariant 6: an
    /// online row is never in a fast tier), report-only.
    #[test]
    fn the_three_network_rows_match_appendix_b() {
        let cases: [(&str, Severity, &[Layer], u32, &str); 3] = [
            (
                ids::NETWORK_GITHUB_RATE_LIMITED,
                Severity::P2,
                &[Layer::Config],
                500,
                "GitHub's REST API budget is spent",
            ),
            (
                ids::NETWORK_CREDENTIAL_REJECTED,
                Severity::P1,
                &[Layer::Config],
                500,
                "GitHub rejects the configured token",
            ),
            (
                ids::NETWORK_SOURCE_UNREACHABLE,
                Severity::P2,
                &[Layer::Config, Layer::Records],
                1_500,
                "an addon source does not answer",
            ),
        ];
        for (id, severity, requires, cost_ms, title) in cases {
            let row = row(id).unwrap();
            assert_eq!(
                (
                    row.subsystem,
                    row.severity,
                    row.tier,
                    row.online,
                    row.requires,
                    row.cost_ms,
                    row.title
                ),
                (
                    Subsystem::Network,
                    severity,
                    Tier::Default,
                    true,
                    requires,
                    cost_ms,
                    title
                ),
                "{id}"
            );
            assert!(row.detect.is_some() && row.fix.is_none(), "{id}");
            let manual = row.manual.unwrap();
            for text in [row.description, row.why, manual.instruction, manual.why] {
                let words: Vec<&str> = text.split_whitespace().collect();
                let names_command = words.windows(2).any(|pair| {
                    pair[0] == "omaddons"
                        && crate::app::COMMANDS
                            .iter()
                            .any(|c| c.name.split_whitespace().next() == Some(pair[1]))
                });
                // A token goes on standard input, never argv: `auth github set` is prose.
                assert!(
                    !names_command || text.contains("omaddons auth github set"),
                    "{id}: names a runnable command: {text}"
                );
            }
        }
        assert!(invariant_violations(FAILURE_MODES).is_empty());
    }

    /// Appendix B row 41: `secrets`' one row, cached, with the chmod fixer.
    #[test]
    fn the_secrets_row_matches_appendix_b() {
        // ubs:ignore — a failure-mode row, not a secret
        let row = row(ids::CREDENTIAL_FILE_MODE).unwrap();
        assert_eq!(
            (
                row.subsystem,
                row.severity,
                row.tier,
                row.online,
                row.cost_ms,
                row.requires,
                row.title
            ),
            (
                Subsystem::Secrets,
                Severity::P2,
                Tier::Cached,
                false,
                1,
                &[Layer::Credentials][..],
                "a credential file is readable by other users"
            )
        );
        assert!(
            row.why
                .contains("undoing that repair restores the old mode, and with it the exposure"),
            "{}",
            row.why
        );
        let spec = row.fix.unwrap();
        assert_eq!(
            (
                spec.writes_to,
                spec.ops,
                spec.enabled,
                spec.needs_force,
                spec.cardinality,
                spec.preconditions
            ),
            (
                &[ScopeName::Config][..],
                &[FixKind::Chmod][..],
                true,
                false,
                Cardinality::Low,
                &[FixerPrecondition::LockHeld][..]
            )
        );
        assert!(row.manual.is_some());
        assert!(invariant_violations(FAILURE_MODES).is_empty());
    }

    /// Appendix B rows 38 and 39: `plugins`' widget rows, reported and never repaired
    /// (M4-D50), each remediated by `omarchy install`, or by prose for a widget serving
    /// another state.
    #[test]
    fn the_plugins_rows_match_appendix_b() {
        for (id, severity, tier, cost_ms, title) in [
            (
                ids::WIDGET_INVALID,
                Severity::P2,
                Tier::Quick,
                2,
                "the installed bar widget fails Omarchy's plugin validation",
            ),
            (
                ids::WIDGET_VERSION_DRIFT,
                Severity::P3,
                Tier::Cached,
                1,
                "the bar widget is from another omaddons build",
            ),
        ] {
            let row = row(id).unwrap();
            assert_eq!(
                (
                    row.subsystem,
                    row.severity,
                    row.tier,
                    row.online,
                    row.cost_ms,
                    row.requires,
                    row.title,
                    row.fix.is_none(),
                    row.manual.is_some(),
                ),
                (
                    Subsystem::Plugins,
                    severity,
                    tier,
                    false,
                    cost_ms,
                    &[Layer::Widget][..],
                    title,
                    true,
                    true,
                ),
                "{id}"
            );
            assert!(
                row.why.contains("cannot be undone"),
                "{id}: never reinstalled by the doctor: {}",
                row.why
            );
            let install = argv_of(&["omaddons", "omarchy", "install"]);
            let ours = row.finding(Observation {
                subject: Subject::machine(),
                title: "t".to_owned(),
                confidence: Confidence::High,
                addon: None,
                folder: None,
                evidence: Evidence::default(),
            });
            assert_eq!(ours.remediation.argv, Some(install), "{id}");
            assert!(
                !ours.remediation.auto_fixable && !ours.remediation.destructive,
                "{id}"
            );
        }
        let row = row(ids::WIDGET_INVALID).unwrap();
        let theirs = row.finding(Observation {
            subject: Subject::machine(),
            title: "t".to_owned(),
            confidence: Confidence::High,
            addon: None,
            folder: None,
            evidence: Evidence {
                reason: Some(plugins::OTHER_CONTEXT),
                ..Evidence::default()
            },
        });
        assert_eq!(theirs.remediation.argv, None, "{:?}", theirs.remediation);
        assert_eq!(
            theirs.remediation.command,
            "reinstall the widget with the omaddons binary and the HOME and XDG directories it was installed under"
        );
        assert!(invariant_violations(FAILURE_MODES).is_empty());
    }

    /// `plugins`' skill row (M4-D64): report-only, `cached`, reading the Skill layer; its
    /// remediation reinstalls the skill (`--dir` for a folder other than the default), and
    /// a link, a non-file or an unreadable file get prose naming no runnable command.
    #[test]
    fn the_skill_drift_row_reports_and_reinstalls() {
        let row = row(ids::SKILL_VERSION_DRIFT).unwrap();
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
                Subsystem::Plugins,
                Severity::P3,
                Tier::Cached,
                false,
                1,
                &[Layer::Skill][..],
                true,
                true,
            )
        );
        let finding = |file: &std::path::Path, reason: &'static str| {
            row.finding(Observation {
                subject: Subject::machine().path(file),
                title: "t".to_owned(),
                confidence: Confidence::High,
                addon: None,
                folder: None,
                evidence: Evidence {
                    path: Some(file.to_path_buf()),
                    reason: Some(reason),
                    ..Evidence::default()
                },
            })
        };
        let default = crate::state::paths::Paths::from_env()
            .unwrap()
            .skill_dir()
            .join("SKILL.md");
        let custom = std::path::Path::new("/x/skills/omaddons/SKILL.md");
        for (case, file, reason, argv) in [
            (
                "default_outdated",
                default.as_path(),
                plugins::SKILL_OUTDATED,
                Some(argv_of(&["omaddons", "robot-docs", "install-skill"])),
            ),
            (
                "custom_edited",
                custom,
                plugins::SKILL_EDITED,
                Some(argv_of(&[
                    "omaddons",
                    "robot-docs",
                    "install-skill",
                    "--dir",
                    "/x/skills/omaddons",
                ])),
            ),
            ("symlink", custom, plugins::SKILL_SYMLINK, None),
            ("not_a_file", custom, plugins::SKILL_NOT_A_FILE, None),
            ("unreadable", custom, plugins::SKILL_UNREADABLE, None),
        ] {
            let found = finding(file, reason);
            let remediation = &found.remediation;
            assert_eq!(remediation.argv, argv, "{case}: {remediation:?}");
            assert!(
                !remediation.auto_fixable && !remediation.destructive,
                "{case}: {remediation:?}"
            );
            if argv.is_none() {
                assert!(
                    !remediation.command.contains("omaddons "),
                    "{case}: prose names no runnable command: {}",
                    remediation.command
                );
            }
        }
    }

    /// An online check that got no answer it could judge is asked again, online.
    #[test]
    fn unanswered_limit_asks_again() {
        let row = row(ids::CHECK_INCOMPLETE).unwrap();
        let f = row.finding(Observation {
            subject: Subject::machine().detector(ids::NETWORK_SOURCE_UNREACHABLE),
            title: "t".to_owned(),
            confidence: Confidence::High,
            addon: None,
            folder: None,
            evidence: Evidence {
                detector: Some(ids::NETWORK_SOURCE_UNREACHABLE),
                reason: Some("unanswered"),
                ..Evidence::default()
            },
        });
        let argv = argv_of(&[
            "omaddons",
            "doctor",
            "--online",
            "--only",
            ids::NETWORK_SOURCE_UNREACHABLE,
        ]);
        assert_eq!(
            (f.remediation.command.as_str(), f.remediation.argv.as_ref()),
            (
                "omaddons doctor --online --only fm-network-source-unreachable",
                Some(&argv)
            )
        );
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
            ..Preloaded::default()
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
        let cases: [(Layer, &str, &[Layer]); 12] = [
            (Layer::Config, "config", &[]),
            (Layer::Registry, "registry", &[]),
            (
                Layer::Discovery,
                "discovery",
                &[Layer::Config, Layer::Registry],
            ),
            (Layer::Records, "records", &[]),
            (
                Layer::Inventory,
                "inventory",
                &[Layer::Discovery, Layer::Records],
            ),
            (Layer::Journals, "journals", &[]),
            (Layer::Lock, "lock", &[]),
            (Layer::History, "history", &[]),
            (Layer::Caches, "caches", &[]),
            (Layer::Widget, "widget", &[]),
            (Layer::Skill, "skill", &[]),
            (Layer::Credentials, "credentials", &[Layer::Config]),
        ];
        let order: Vec<Layer> = cases.iter().map(|(layer, ..)| *layer).collect();
        assert_eq!(Layer::ALL.to_vec(), order, "the order");
        for (layer, wire, expected) in cases {
            assert_eq!(json!(layer), json!(wire), "{layer:?}");
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
        let journal = |on_target: bool| {
            let row = row(ids::JOURNAL_PENDING).unwrap();
            let mut f = finding(row, on_target);
            f.evidence = Evidence {
                tx: Some(tx),
                ..Evidence::default()
            };
            row.remediation(&f)
        };
        // `journal-pending` is only a journal recovery settles (F1), and its fix is the
        // delegation (F2): on a client with `-t`, and, for a doctor repair, on the
        // machine. Stuck states have rows of their own, remediated by their `doctor
        // explain`.
        let on_client = journal(true);
        assert_eq!(
            on_client.argv,
            Some(argv_of(&[
                "omaddons",
                "doctor",
                "fix",
                "--only",
                ids::JOURNAL_PENDING,
                "-t",
                "retail"
            ])),
            "{on_client:?}"
        );
        assert!(on_client.auto_fixable && !on_client.needs_force);
        assert_eq!(
            journal(false).argv,
            Some(argv_of(&[
                "omaddons",
                "doctor",
                "fix",
                "--only",
                ids::JOURNAL_PENDING
            ]))
        );
        let mut stuck = finding(row(ids::JOURNAL_TOO_NEW).unwrap(), true);
        stuck.target = Some(target.clone());
        assert_eq!(
            explain_manual(&stuck).argv,
            Some(argv_of(&[
                "omaddons",
                "doctor",
                "explain",
                ids::JOURNAL_TOO_NEW,
                "-t",
                "retail"
            ]))
        );
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

    fn ids_of(rows: &[FailureMode]) -> Vec<&'static str> {
        rows.iter().map(|row| row.id).collect()
    }

    #[test]
    fn test_rows_only_with_the_switch() {
        let catalog = ids_of(FAILURE_MODES);
        let without = ids_of(&failure_modes_with(false));
        assert_eq!(without, catalog, "without the switch: the catalog");
        let with = ids_of(&failure_modes_with(true));
        assert_eq!(
            &with[..catalog.len()],
            catalog.as_slice(),
            "the catalog first"
        );
        assert_eq!(
            &with[catalog.len()..],
            [
                "fm-test-write",
                "fm-test-quarantine",
                "fm-test-flatten",
                "fm-test-chmod",
                "fm-test-suspend",
            ],
            "then the test rows, in their order"
        );
        // Unit tests never set the variable: this process knows the catalog alone.
        assert_eq!(ids_of(failure_modes()), catalog);
    }

    #[test]
    fn test_rows_obey_the_invariants() {
        let both = failure_modes_with(true);
        assert_eq!(invariant_violations(&both), []);
        assert!(
            FAILURE_MODES
                .iter()
                .all(|row| !row.id.starts_with(TEST_ID_PREFIX)),
            "no catalog id starts {TEST_ID_PREFIX}"
        );
        // The gates the end-to-end suite drives: an ordinary auto-fixer, a forced one,
        // and a disabled one refused while the client runs; E7's chmod, an ordinary one,
        // and its set-aside, forced and refused while the client runs, as F2's will be.
        let gates: Vec<(&str, bool, bool, bool, Tier)> = both[FAILURE_MODES.len()..]
            .iter()
            .map(|row| {
                let fix = row.fix.unwrap();
                let closed = fix
                    .preconditions
                    .contains(&FixerPrecondition::ClientNotRunning);
                (row.id, fix.enabled, fix.needs_force, closed, row.tier)
            })
            .collect();
        assert_eq!(
            gates,
            [
                ("fm-test-write", true, false, false, Tier::Default),
                ("fm-test-quarantine", false, true, false, Tier::Default),
                ("fm-test-flatten", false, false, true, Tier::Default),
                ("fm-test-chmod", true, false, false, Tier::Default),
                ("fm-test-suspend", false, true, true, Tier::Default),
            ]
        );
        // The exemptions are the prefix's alone: a catalog row out of order, or naming
        // another subsystem, still breaks the rules.
        let mut misplaced = both.to_vec();
        misplaced.swap(0, 1);
        assert!(!invariant_violations(&misplaced).is_empty());
    }
}
