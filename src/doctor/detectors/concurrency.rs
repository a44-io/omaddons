//! `concurrency_primitives`: interrupted transactions and the global lock. Every journal
//! row reads E11's classifier (`journal::diagnose`) through one pure mapping, so each
//! state recovery leaves a journal in has its own failure mode, title and evidence.

use jiff::Timestamp;

use crate::doctor::attribution::attribute;
use crate::doctor::budget::Budget;
use crate::doctor::finding::{
    Confidence, Detected, Evidence, Incomplete, IncompleteLimit, Observation, Subject,
};
use crate::doctor::registry::ids;
use crate::doctor::selection::Selection;
use crate::doctor::view::{ArtifactCounts, DoctorView, Journals};
use crate::engine::journal::{Phase, StopReason};
use crate::engine::repair::journal::RepairPhase;
use crate::engine::repair_action::{JournalFamily, JournalPhase};
use crate::engine::simulate::{JournalDiagnosis, JournalVerdict, UnattributedCause};
use crate::fsx::LockState;
use crate::limits::{DOCTOR_SCAN_MAX_ENTRIES, LOCK_HELD_LONG};
use crate::model::TargetKey;

const WAITS: &str = "An interrupted change waits to be finished";

// ---------------------------------------------------------------------------------------
// The journal rows (Appendix B rows 1-9).

/// What the journal rows read: the Journals layer, the lock (`None` when that layer is
/// unavailable, which counts as not held), and how a target key reads.
pub struct JournalInput<'a> {
    pub journals: &'a Journals,
    pub lock: Option<&'a LockState>,
    /// The view was built under the global lock its own process holds.
    pub caller_holds_lock: bool,
    /// The registry id, else the key's string (`DoctorView::target_ref`'s rule).
    pub target_id: &'a dyn Fn(TargetKey) -> String,
}

/// Every journal row's observations, each with its row's id: none while another process
/// holds the global lock (it is committing, or about to recover these journals), never a
/// live journal (in progress) nor a committed one (recovery only deletes it).
pub fn journal_observations(input: &JournalInput<'_>) -> Vec<(&'static str, Observation)> {
    if another_holds_the_lock(input) {
        return Vec::new();
    }
    input
        .journals
        .diagnoses
        .journals
        .iter()
        .filter_map(|d| journal_observation(d, input.journals.counts.get(&d.tx), input.target_id))
        .collect()
}

/// `incomplete` for row `id` when a count of one of its own journals was cut short.
pub fn journal_incomplete(input: &JournalInput<'_>, id: &str) -> Option<Incomplete> {
    let truncated = journal_observations(input).iter().any(|(row, obs)| {
        *row == id
            && obs
                .evidence
                .tx
                .and_then(|tx| input.journals.counts.get(&tx))
                .is_some_and(|counts| counts.truncated)
    });
    truncated.then_some(Incomplete {
        limit: IncompleteLimit::Entries,
        deadline_ms: None,
        max_entries: Some(DOCTOR_SCAN_MAX_ENTRIES),
    })
}

fn another_holds_the_lock(input: &JournalInput<'_>) -> bool {
    matches!(input.lock, Some(LockState::Held { .. })) && !input.caller_holds_lock
}

/// One journal's row and observation; `None` when it is no finding.
fn journal_observation(
    d: &JournalDiagnosis,
    counts: Option<&ArtifactCounts>,
    target_id: &dyn Fn(TargetKey) -> String,
) -> Option<(&'static str, Observation)> {
    if d.live {
        return None;
    }
    let repair = d.family == JournalFamily::Repair;
    let mut evidence = Evidence::default();
    // A journal behind a barrier is reported under its own state (E14); what holds it
    // back is its owner's own finding.
    let verdict = match d.verdict {
        JournalVerdict::BehindBarrier { .. } => d.evidence.held.unwrap_or(d.verdict),
        verdict => verdict,
    };
    let (id, reason, title) = match verdict {
        JournalVerdict::Recoverable {
            phase: Phase::Committed,
            ..
        } => return None,
        // Behind a barrier with no own verdict kept: it waits, as a pending journal does.
        JournalVerdict::Recoverable { .. } | JournalVerdict::BehindBarrier { .. } => (
            ids::JOURNAL_PENDING,
            None,
            if repair {
                "An interrupted doctor repair waits to be finished".to_owned()
            } else {
                WAITS.to_owned()
            },
        ),
        JournalVerdict::WaitsForGame => (
            ids::JOURNAL_WAITS_FOR_GAME,
            Some(StopReason::SavedVariables),
            "An interrupted change to WTF/ waits for the game to exit".to_owned(),
        ),
        JournalVerdict::TooNew => (
            ids::JOURNAL_TOO_NEW,
            Some(StopReason::JournalTooNew),
            too_new_title(d),
        ),
        JournalVerdict::Unreadable => (
            ids::JOURNAL_UNREADABLE,
            Some(StopReason::JournalUnreadable),
            "An interrupted change's journal holds lines this omaddons cannot read".to_owned(),
        ),
        JournalVerdict::UnexpectedState { step } => {
            evidence.step = Some(step);
            (
                ids::JOURNAL_UNEXPECTED_STATE,
                Some(StopReason::UnexpectedState),
                format!(
                    "An interrupted change to WTF/ stopped at step {step}, which it does not recognize"
                ),
            )
        }
        JournalVerdict::StepUnrecognized { step } => {
            evidence.step = Some(step);
            let what = if repair {
                "An interrupted doctor repair"
            } else {
                "An interrupted change"
            };
            (
                ids::JOURNAL_STEP_UNRECOGNIZED,
                Some(StopReason::StepUnrecognized),
                format!(
                    "{what} stopped at step {step}, which matches neither its before nor its after state"
                ),
            )
        }
        JournalVerdict::MarkerMismatch => {
            evidence.path.clone_from(&d.client_dir);
            evidence.expected = d.target.map(|key| key.to_string());
            evidence.observed = d.evidence.marker.map(|key| key.to_string());
            (
                ids::JOURNAL_MARKER_MISMATCH,
                Some(StopReason::MarkerMismatch),
                "An interrupted change names a client directory that is no longer this client's"
                    .to_owned(),
            )
        }
        // A repair's own scope refusal, as a client's marker is (both `scope_denied`).
        JournalVerdict::SkillDirUnattributed => (
            ids::JOURNAL_MARKER_MISMATCH,
            Some(StopReason::SkillDir),
            "An interrupted doctor repair names a skill directory omaddons cannot attribute"
                .to_owned(),
        ),
        JournalVerdict::PreservationIncomplete { stops } => {
            evidence.resumable = Some(!stops);
            evidence.count = d.evidence.preservation.map(|p| p.items as u64);
            (
                ids::JOURNAL_PRESERVATION_INCOMPLETE,
                Some(StopReason::PreservationIncomplete),
                if stops {
                    "An interrupted change to WTF/ could not finish keeping the settings it displaced"
                } else {
                    "An interrupted change to WTF/ is still keeping the settings it displaced"
                }
                .to_owned(),
            )
        }
        JournalVerdict::Unattributed { cause } => {
            evidence.cause = Some(cause_name(cause));
            (
                ids::JOURNAL_UNATTRIBUTED,
                Some(StopReason::Unattributed),
                "An interrupted change cannot be attributed to any client, so every change is refused"
                    .to_owned(),
            )
        }
    };
    let unattributed = id == ids::JOURNAL_UNATTRIBUTED;
    let subject = match d.target {
        Some(key) if !unattributed => Subject::target(&target_id(key), key),
        _ => Subject::machine(),
    };
    let trusted = !matches!(
        verdict,
        JournalVerdict::TooNew | JournalVerdict::Unreadable | JournalVerdict::Unattributed { .. }
    );
    let counts = counts.copied().unwrap_or_default();
    let evidence = Evidence {
        tx: Some(d.tx),
        reason: reason.map(StopReason::as_str),
        family: Some(d.family),
        kind: d.evidence.kind,
        phase: if trusted { phase_of(d) } else { None },
        journal: Some(d.path.clone()),
        format: d.evidence.version.map(|v| v.to_string()),
        target: if unattributed { None } else { d.target },
        stage_folders: Some(counts.stage_folders),
        snapshot_folders: Some(counts.snapshot_folders),
        may_hold_only_copy: Some(counts.stage_folders > 0 || counts.snapshot_folders > 0),
        ..evidence
    };
    Some((
        id,
        Observation {
            subject: subject.tx(d.tx),
            title,
            confidence: if d.exact() {
                Confidence::High
            } else {
                Confidence::Medium
            },
            addon: None,
            folder: None,
            evidence,
        },
    ))
}

/// `journal-too-new`'s title: its kind and format when the journal gives them.
fn too_new_title(d: &JournalDiagnosis) -> String {
    let kind = d
        .evidence
        .kind
        .and_then(|kind| serde_json::to_value(kind).ok())
        .and_then(|kind| kind.as_str().map(|k| format!(" ({k})")))
        .unwrap_or_default();
    let format = d
        .evidence
        .version
        .map(|v| format!(" (format {v})"))
        .unwrap_or_default();
    format!(
        "An interrupted change{kind} was started by a newer omaddons; this build cannot read its journal{format}"
    )
}

fn cause_name(cause: UnattributedCause) -> &'static str {
    match cause {
        UnattributedCause::LivenessUnknown => "liveness_unknown",
        UnattributedCause::ReadFailed => "read_failed",
        UnattributedCause::TooNew => "too_new",
        UnattributedCause::Unreadable => "unreadable",
    }
}

/// Where a journal whose lines can be trusted stands; `None` once it is settled.
fn phase_of(d: &JournalDiagnosis) -> Option<JournalPhase> {
    if d.family == JournalFamily::Repair {
        return match d.repair_phase? {
            RepairPhase::Unprepared => Some(JournalPhase::Unprepared),
            RepairPhase::Prepared => Some(JournalPhase::Prepared),
            RepairPhase::Stepped(_) => Some(JournalPhase::Step),
            RepairPhase::Done => Some(JournalPhase::Done),
            RepairPhase::Committed | RepairPhase::RolledBack => None,
        };
    }
    if let Some(preservation) = d.evidence.preservation
        && preservation.intent.is_some()
    {
        return Some(if preservation.pending {
            JournalPhase::Preserve
        } else {
            JournalPhase::Preserved
        });
    }
    match d.phase {
        Phase::Unprepared => Some(JournalPhase::Unprepared),
        Phase::Prepared if d.evidence.steps == 0 => Some(JournalPhase::Prepared),
        Phase::Prepared => Some(JournalPhase::Step),
        Phase::Swapped => Some(JournalPhase::Swapped),
        Phase::Committed => None,
    }
}

/// `status`'s title for `n > 1` instances of row `id` on one client; `None` for a row
/// with no such title (one lock).
pub fn plural_title(id: &str, n: usize) -> Option<String> {
    let rest = match id {
        ids::JOURNAL_PENDING => "interrupted changes wait to be finished",
        ids::JOURNAL_WAITS_FOR_GAME => "interrupted changes to WTF/ wait for the game to exit",
        ids::JOURNAL_TOO_NEW => "interrupted changes were started by a newer omaddons",
        ids::JOURNAL_UNREADABLE => {
            "interrupted changes' journals hold lines this omaddons cannot read"
        }
        ids::JOURNAL_UNEXPECTED_STATE => {
            "interrupted changes to WTF/ stopped at steps they do not recognize"
        }
        ids::JOURNAL_STEP_UNRECOGNIZED => {
            "interrupted changes stopped at steps that match neither their before nor their after state"
        }
        ids::JOURNAL_MARKER_MISMATCH => {
            "interrupted changes name a client directory that is no longer this client's"
        }
        ids::JOURNAL_UNATTRIBUTED => {
            "interrupted changes cannot be attributed to any client, so every change is refused"
        }
        ids::JOURNAL_PRESERVATION_INCOMPLETE => {
            "interrupted changes to WTF/ have not finished keeping the settings they displaced"
        }
        _ => return None,
    };
    Some(format!("{n} {rest}"))
}

/// Row `id`'s detector: the shared mapping, its own observations.
fn detect_row(id: &'static str, view: &DoctorView) -> Detected {
    detect_row_with(id, view, None)
}

/// The stuck rows (Appendix B rows 3-8), whose findings say what a set-aside of them
/// could attribute (F2 s2).
const STUCK: [&str; 6] = [
    ids::JOURNAL_TOO_NEW,
    ids::JOURNAL_UNREADABLE,
    ids::JOURNAL_UNEXPECTED_STATE,
    ids::JOURNAL_STEP_UNRECOGNIZED,
    ids::JOURNAL_MARKER_MISMATCH,
    ids::JOURNAL_UNATTRIBUTED,
];

/// A stuck row's detector: [`detect_row`], each finding with its `attribution` and
/// `unattributed_dirs` (each journal's payload read under `budget`, whose cut is the
/// detector's `incomplete`).
fn detect_stuck(id: &'static str, view: &DoctorView, budget: &mut Budget) -> Detected {
    detect_row_with(id, view, Some(budget))
}

fn detect_row_with(id: &'static str, view: &DoctorView, budget: Option<&mut Budget>) -> Detected {
    let Some(journals) = view.journals() else {
        return Detected::default();
    };
    let target_id = |key: TargetKey| view.target_ref(key).id;
    let input = JournalInput {
        journals,
        lock: view.lock(),
        caller_holds_lock: view.caller_holds_lock(),
        target_id: &target_id,
    };
    let mut found: Vec<Observation> = journal_observations(&input)
        .into_iter()
        .filter(|(row, _)| *row == id)
        .map(|(_, obs)| obs)
        .collect();
    let mut incomplete = journal_incomplete(&input, id);
    if let Some(budget) = budget.filter(|_| STUCK.contains(&id)) {
        for obs in &mut found {
            let Some(d) = journals
                .diagnoses
                .journals
                .iter()
                .find(|d| Some(d.tx) == obs.evidence.tx)
            else {
                continue;
            };
            let a = attribute(view, d, budget);
            obs.evidence.attribution = Some(a.attribution);
            obs.evidence.unattributed_dirs = Some(a.unattributed);
        }
        incomplete = incomplete.or_else(|| budget.incomplete());
    }
    Detected { found, incomplete }
}

/// `journal-pending`: an interrupted change recovery can settle.
pub(crate) fn journal_pending(view: &DoctorView, _: &Selection, _: &mut Budget) -> Detected {
    detect_row(ids::JOURNAL_PENDING, view)
}

pub(crate) fn journal_waits_for_game(view: &DoctorView, _: &Selection, _: &mut Budget) -> Detected {
    detect_row(ids::JOURNAL_WAITS_FOR_GAME, view)
}

pub(crate) fn journal_too_new(view: &DoctorView, _: &Selection, budget: &mut Budget) -> Detected {
    detect_stuck(ids::JOURNAL_TOO_NEW, view, budget)
}

pub(crate) fn journal_unreadable(
    view: &DoctorView,
    _: &Selection,
    budget: &mut Budget,
) -> Detected {
    detect_stuck(ids::JOURNAL_UNREADABLE, view, budget)
}

pub(crate) fn journal_unexpected_state(
    view: &DoctorView,
    _: &Selection,
    budget: &mut Budget,
) -> Detected {
    detect_stuck(ids::JOURNAL_UNEXPECTED_STATE, view, budget)
}

pub(crate) fn journal_step_unrecognized(
    view: &DoctorView,
    _: &Selection,
    budget: &mut Budget,
) -> Detected {
    detect_stuck(ids::JOURNAL_STEP_UNRECOGNIZED, view, budget)
}

pub(crate) fn journal_marker_mismatch(
    view: &DoctorView,
    _: &Selection,
    budget: &mut Budget,
) -> Detected {
    detect_stuck(ids::JOURNAL_MARKER_MISMATCH, view, budget)
}

pub(crate) fn journal_unattributed(
    view: &DoctorView,
    _: &Selection,
    budget: &mut Budget,
) -> Detected {
    detect_stuck(ids::JOURNAL_UNATTRIBUTED, view, budget)
}

pub(crate) fn journal_preservation_incomplete(
    view: &DoctorView,
    _: &Selection,
    _: &mut Budget,
) -> Detected {
    detect_row(ids::JOURNAL_PRESERVATION_INCOMPLETE, view)
}

// ---------------------------------------------------------------------------------------
// `lock-held-long` (row 10).

/// `lock-held-long`, from the Lock layer: never the caller's own hold.
pub(crate) fn lock_held_long(view: &DoctorView, _: &Selection, _: &mut Budget) -> Detected {
    let found = view
        .lock()
        .and_then(|lock| lock_observation(lock, view.caller_holds_lock(), crate::clock::now()))
        .into_iter()
        .collect();
    Detected::complete(found)
}

/// A holder strictly older than `LOCK_HELD_LONG`, by its own record. A record that cannot
/// be read is no finding (nothing to judge), and a `started_at` in the future is age 0.
pub fn lock_observation(
    lock: &LockState,
    caller_holds_lock: bool,
    now: Timestamp,
) -> Option<Observation> {
    if caller_holds_lock {
        return None;
    }
    let LockState::Held { holder: Some(h) } = lock else {
        return None;
    };
    let age_ms = now
        .as_millisecond()
        .saturating_sub(h.started_at.as_millisecond())
        .max(0);
    let long_ms = i64::try_from(LOCK_HELD_LONG.as_millis()).unwrap_or(i64::MAX);
    if age_ms <= long_ms {
        return None;
    }
    Some(Observation {
        subject: Subject::machine(),
        title: format!(
            "An omaddons process (pid {}, {}) has held the lock for more than {} minutes",
            h.pid,
            h.command,
            LOCK_HELD_LONG.as_secs() / 60
        ),
        confidence: Confidence::Medium,
        addon: None,
        folder: None,
        evidence: Evidence {
            pid: Some(h.pid),
            command: Some(h.command.clone()),
            age_s: Some(u64::try_from(age_ms / 1_000).unwrap_or(0)),
            ..Evidence::default()
        },
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use serde_json::{Value, json};

    use super::*;
    use crate::doctor::finding::Finding;
    use crate::doctor::registry::row;
    use crate::engine::simulate::{
        Approximation, Diagnoses, Direction, JournalEvidence, Preservation, ProtectedPaths,
    };
    use crate::fsx::HolderInfo;
    use crate::model::{TxId, TxKind};
    use crate::state::store::SchemaVersion;

    const K: &str = "01M5ZQ7Y3KJ8V9W2X4C6B0N1RT";
    const TX: &str = "01M7A1B2C3D4E5F6G7H8J9K0MN";
    const TX2: &str = "01M7A1B2C3D4E5F6G7H8J9K0MP";

    fn key() -> TargetKey {
        K.parse().unwrap()
    }

    /// A folder journal on retail, prepared, with `verdict`.
    fn diagnosis(verdict: JournalVerdict) -> JournalDiagnosis {
        JournalDiagnosis {
            tx: TX.parse().unwrap(),
            family: JournalFamily::Folder,
            path: PathBuf::from(format!("/state/omaddons/journal/{TX}.jsonl")),
            target: Some(key()),
            client_dir: Some(PathBuf::from("/wow/_retail_")),
            phase: Phase::Prepared,
            repair_phase: None,
            live: false,
            verdict,
            evidence: JournalEvidence {
                version: Some(SchemaVersion::new(1, 1)),
                kind: Some(TxKind::Update),
                ..JournalEvidence::default()
            },
            protected: ProtectedPaths::default(),
        }
    }

    fn journals(diagnoses: Vec<JournalDiagnosis>) -> Journals {
        Journals {
            pending: Vec::new(),
            diagnoses: Diagnoses {
                journals: diagnoses,
                unlisted: None,
            },
            counts: BTreeMap::new(),
        }
    }

    fn retail(key: TargetKey) -> String {
        if key.to_string() == K {
            "retail".to_owned()
        } else {
            key.to_string()
        }
    }

    /// Every finding `journals` gives under `lock`, as its row reports it.
    fn findings_under(journals: &Journals, lock: Option<&LockState>, own: bool) -> Vec<Value> {
        let input = JournalInput {
            journals,
            lock,
            caller_holds_lock: own,
            target_id: &retail,
        };
        journal_observations(&input)
            .into_iter()
            .map(|(id, obs)| {
                assert!(obs.evidence.is_private_safe(), "{id}: {:?}", obs.evidence);
                let finding: Finding = row(id).unwrap().finding(obs);
                serde_json::to_value(&finding).unwrap()
            })
            .collect()
    }

    fn findings(diagnoses: Vec<JournalDiagnosis>) -> Vec<Value> {
        findings_under(&journals(diagnoses), None, false)
    }

    fn pretty(value: &impl serde::Serialize) -> String {
        serde_json::to_string_pretty(value).unwrap_or_default()
    }

    #[test]
    fn each_verdict_maps_to_its_row() {
        let repair = |verdict, phase| JournalDiagnosis {
            family: JournalFamily::Repair,
            path: PathBuf::from(format!("/state/omaddons/journal/{TX}.repair.jsonl")),
            target: None,
            client_dir: None,
            repair_phase: Some(phase),
            ..diagnosis(verdict)
        };
        let unprepared = JournalDiagnosis {
            phase: Phase::Unprepared,
            evidence: JournalEvidence::default(),
            ..diagnosis(JournalVerdict::Recoverable {
                phase: Phase::Unprepared,
                direction: Direction::Prune,
            })
        };
        let without_kind = JournalDiagnosis {
            evidence: JournalEvidence {
                version: Some(SchemaVersion::new(9, 0)),
                ..JournalEvidence::default()
            },
            ..diagnosis(JournalVerdict::TooNew)
        };
        let unattributed = |cause| JournalDiagnosis {
            target: None,
            ..diagnosis(JournalVerdict::Unattributed { cause })
        };
        let preservation = |stops, pending| JournalDiagnosis {
            evidence: JournalEvidence {
                preservation: Some(Preservation {
                    intent: Some("01M7A1B2C3D4E5F6G7H8J9K0MQ".parse().unwrap()),
                    items: 4,
                    pending,
                }),
                ..diagnosis(JournalVerdict::WaitsForGame).evidence
            },
            ..diagnosis(JournalVerdict::PreservationIncomplete { stops })
        };
        let swapped = JournalDiagnosis {
            phase: Phase::Swapped,
            ..diagnosis(JournalVerdict::Recoverable {
                phase: Phase::Swapped,
                direction: Direction::Forward,
            })
        };
        let stepped = JournalDiagnosis {
            evidence: JournalEvidence {
                steps: 3,
                ..diagnosis(JournalVerdict::WaitsForGame).evidence
            },
            ..diagnosis(JournalVerdict::UnexpectedState { step: 3 })
        };
        let other_marker = JournalDiagnosis {
            evidence: JournalEvidence {
                marker: Some("01M5ZQ7Y3KJ8V9W2X4C6B0N1RV".parse().unwrap()),
                ..diagnosis(JournalVerdict::MarkerMismatch).evidence
            },
            ..diagnosis(JournalVerdict::MarkerMismatch)
        };
        // (case, the diagnosis, its row, the instance's scope, title, evidence keys)
        let on_retail = format!("{K}:tx={TX}");
        let on_machine = format!("machine:tx={TX}");
        type Case = (
            &'static str,
            JournalDiagnosis,
            &'static str,
            String,
            String,
            Value,
        );
        let cases: Vec<Case> = vec![
            (
                "recoverable_prepared_back",
                diagnosis(JournalVerdict::Recoverable {
                    phase: Phase::Prepared,
                    direction: Direction::Back,
                }),
                ids::JOURNAL_PENDING,
                on_retail.clone(),
                "An interrupted change waits to be finished".to_owned(),
                json!({"phase": "prepared", "family": "folder", "kind": "update", "format": "1.1", "target": K}),
            ),
            (
                "recoverable_swapped_forward",
                swapped,
                ids::JOURNAL_PENDING,
                on_retail.clone(),
                "An interrupted change waits to be finished".to_owned(),
                json!({"phase": "swapped"}),
            ),
            (
                "recoverable_unprepared_prune",
                unprepared,
                ids::JOURNAL_PENDING,
                on_retail.clone(),
                "An interrupted change waits to be finished".to_owned(),
                json!({"phase": "unprepared"}),
            ),
            (
                "waits_for_game",
                diagnosis(JournalVerdict::WaitsForGame),
                ids::JOURNAL_WAITS_FOR_GAME,
                on_retail.clone(),
                "An interrupted change to WTF/ waits for the game to exit".to_owned(),
                json!({"reason": "saved_variables", "phase": "prepared"}),
            ),
            (
                "too_new",
                JournalDiagnosis {
                    evidence: JournalEvidence {
                        version: Some(SchemaVersion::new(9, 0)),
                        kind: Some(TxKind::Update),
                        ..JournalEvidence::default()
                    },
                    ..diagnosis(JournalVerdict::TooNew)
                },
                ids::JOURNAL_TOO_NEW,
                on_retail.clone(),
                "An interrupted change (update) was started by a newer omaddons; this build cannot read its journal (format 9.0)".to_owned(),
                json!({"reason": "journal_too_new", "format": "9.0"}),
            ),
            (
                "too_new_without_kind",
                without_kind,
                ids::JOURNAL_TOO_NEW,
                on_retail.clone(),
                "An interrupted change was started by a newer omaddons; this build cannot read its journal (format 9.0)".to_owned(),
                json!({"reason": "journal_too_new"}),
            ),
            (
                "unreadable",
                diagnosis(JournalVerdict::Unreadable),
                ids::JOURNAL_UNREADABLE,
                on_retail.clone(),
                "An interrupted change's journal holds lines this omaddons cannot read".to_owned(),
                json!({"reason": "journal_unreadable"}),
            ),
            (
                "unexpected_state_at_step_3",
                stepped,
                ids::JOURNAL_UNEXPECTED_STATE,
                on_retail.clone(),
                "An interrupted change to WTF/ stopped at step 3, which it does not recognize"
                    .to_owned(),
                json!({"reason": "unexpected_state", "step": 3, "phase": "step"}),
            ),
            (
                "step_unrecognized_at_step_2",
                diagnosis(JournalVerdict::StepUnrecognized { step: 2 }),
                ids::JOURNAL_STEP_UNRECOGNIZED,
                on_retail.clone(),
                "An interrupted change stopped at step 2, which matches neither its before nor its after state".to_owned(),
                json!({"reason": "step_unrecognized", "step": 2}),
            ),
            (
                "marker_mismatch_other_key",
                other_marker,
                ids::JOURNAL_MARKER_MISMATCH,
                on_retail.clone(),
                "An interrupted change names a client directory that is no longer this client's"
                    .to_owned(),
                json!({"reason": "marker_mismatch", "path": "/wow/_retail_", "expected": K, "observed": "01M5ZQ7Y3KJ8V9W2X4C6B0N1RV"}),
            ),
            (
                "marker_mismatch_dir_gone",
                diagnosis(JournalVerdict::MarkerMismatch),
                ids::JOURNAL_MARKER_MISMATCH,
                on_retail.clone(),
                "An interrupted change names a client directory that is no longer this client's"
                    .to_owned(),
                json!({"reason": "marker_mismatch", "expected": K}),
            ),
            (
                "unattributed_liveness_unknown",
                unattributed(UnattributedCause::LivenessUnknown),
                ids::JOURNAL_UNATTRIBUTED,
                on_machine.clone(),
                "An interrupted change cannot be attributed to any client, so every change is refused".to_owned(),
                json!({"reason": "unattributed", "cause": "liveness_unknown"}),
            ),
            (
                "unattributed_read_failed",
                unattributed(UnattributedCause::ReadFailed),
                ids::JOURNAL_UNATTRIBUTED,
                on_machine.clone(),
                "An interrupted change cannot be attributed to any client, so every change is refused".to_owned(),
                json!({"cause": "read_failed"}),
            ),
            (
                "unattributed_too_new",
                unattributed(UnattributedCause::TooNew),
                ids::JOURNAL_UNATTRIBUTED,
                on_machine.clone(),
                "An interrupted change cannot be attributed to any client, so every change is refused".to_owned(),
                json!({"cause": "too_new"}),
            ),
            (
                "unattributed_unreadable",
                unattributed(UnattributedCause::Unreadable),
                ids::JOURNAL_UNATTRIBUTED,
                on_machine.clone(),
                "An interrupted change cannot be attributed to any client, so every change is refused".to_owned(),
                json!({"cause": "unreadable"}),
            ),
            (
                "preservation_incomplete_resumable",
                preservation(false, true),
                ids::JOURNAL_PRESERVATION_INCOMPLETE,
                on_retail.clone(),
                "An interrupted change to WTF/ is still keeping the settings it displaced"
                    .to_owned(),
                json!({"reason": "preservation_incomplete", "resumable": true, "count": 4, "phase": "preserve"}),
            ),
            (
                "preservation_incomplete_stops",
                preservation(true, true),
                ids::JOURNAL_PRESERVATION_INCOMPLETE,
                on_retail.clone(),
                "An interrupted change to WTF/ could not finish keeping the settings it displaced"
                    .to_owned(),
                json!({"resumable": false}),
            ),
            (
                "repair_recoverable",
                repair(
                    JournalVerdict::Recoverable {
                        phase: Phase::Swapped,
                        direction: Direction::Forward,
                    },
                    RepairPhase::Done,
                ),
                ids::JOURNAL_PENDING,
                on_machine.clone(),
                "An interrupted doctor repair waits to be finished".to_owned(),
                json!({"family": "repair", "phase": "done"}),
            ),
            (
                "repair_step_unrecognized",
                repair(
                    JournalVerdict::StepUnrecognized { step: 1 },
                    RepairPhase::Stepped(1),
                ),
                ids::JOURNAL_STEP_UNRECOGNIZED,
                on_machine.clone(),
                "An interrupted doctor repair stopped at step 1, which matches neither its before nor its after state".to_owned(),
                json!({"family": "repair", "phase": "step", "step": 1}),
            ),
            (
                "repair_skill_dir",
                repair(JournalVerdict::SkillDirUnattributed, RepairPhase::Prepared),
                ids::JOURNAL_MARKER_MISMATCH,
                on_machine.clone(),
                "An interrupted doctor repair names a skill directory omaddons cannot attribute"
                    .to_owned(),
                json!({"reason": "skill_dir", "family": "repair"}),
            ),
        ];
        for (case, d, id, scope, title, evidence) in cases {
            let got = findings(vec![d.clone()]);
            let ctx = format!("{case}\n{}\n{}", pretty(&d), pretty(&got));
            assert_eq!(got.len(), 1, "{ctx}");
            let f = &got[0];
            assert_eq!(f["id"], id, "{ctx}");
            assert_eq!(f["instance"], format!("{id}@{scope}"), "{ctx}");
            assert_eq!(f["title"], title, "{ctx}");
            assert_eq!(f["confidence"], "high", "{ctx}");
            assert_eq!(f["evidence"]["tx"], TX, "{ctx}");
            assert!(f["evidence"]["journal"].as_str().is_some(), "{ctx}");
            for (key, want) in evidence.as_object().unwrap() {
                assert_eq!(&f["evidence"][key], want, "{ctx}: evidence.{key}");
            }
            let label = if scope.starts_with("machine") {
                format!("machine · tx {TX}")
            } else {
                format!("retail · tx {TX}")
            };
            assert_eq!(f["label"], label, "{ctx}");
        }
        assert!(
            findings(vec![diagnosis(JournalVerdict::Recoverable {
                phase: Phase::Committed,
                direction: Direction::Prune,
            })])
            .is_empty(),
            "committed_is_no_finding"
        );
    }

    #[test]
    fn untrusted_journals_carry_no_phase_and_unattributed_no_target() {
        for verdict in [
            JournalVerdict::TooNew,
            JournalVerdict::Unreadable,
            JournalVerdict::Unattributed {
                cause: UnattributedCause::Unreadable,
            },
        ] {
            let got = findings(vec![diagnosis(verdict)]);
            assert_eq!(got[0]["evidence"].get("phase"), None, "{verdict:?}");
        }
        let got = findings(vec![diagnosis(JournalVerdict::Unattributed {
            cause: UnattributedCause::ReadFailed,
        })]);
        assert_eq!(got[0]["evidence"].get("target"), None);
        assert_eq!(got[0]["target"], Value::Null, "machine scope");
    }

    #[test]
    fn the_lock_decides_what_is_hidden() {
        let stuck = || {
            journals(vec![
                diagnosis(JournalVerdict::Unreadable),
                JournalDiagnosis {
                    tx: TX2.parse().unwrap(),
                    ..diagnosis(JournalVerdict::WaitsForGame)
                },
            ])
        };
        let held = LockState::Held {
            holder: Some(HolderInfo {
                pid: 7,
                command: "omaddons update".to_owned(),
                started_at: "2026-10-31T08:00:00Z".parse().unwrap(),
            }),
        };
        assert!(
            findings_under(&stuck(), Some(&held), false).is_empty(),
            "another_holder_hides_every_journal"
        );
        assert_eq!(
            findings_under(&stuck(), Some(&held), true).len(),
            2,
            "the_callers_own_lock_hides_nothing"
        );
        for (case, lock) in [
            ("free", Some(LockState::Free)),
            ("missing", Some(LockState::Missing)),
            ("unavailable", None),
        ] {
            assert_eq!(
                findings_under(&stuck(), lock.as_ref(), false).len(),
                2,
                "{case} hides nothing"
            );
        }
        let live = JournalDiagnosis {
            live: true,
            ..diagnosis(JournalVerdict::Unreadable)
        };
        assert!(
            findings(vec![live]).is_empty(),
            "a_live_journal_is_no_finding"
        );
    }

    #[test]
    fn instances_scopes_and_explain_argvs() {
        let two = findings(vec![
            diagnosis(JournalVerdict::Unreadable),
            JournalDiagnosis {
                tx: TX2.parse().unwrap(),
                ..diagnosis(JournalVerdict::Unreadable)
            },
        ]);
        let instances: Vec<&str> = two
            .iter()
            .map(|f| f["instance"].as_str().unwrap())
            .collect();
        assert_eq!(
            instances,
            [
                format!("{}@{K}:tx={TX}", ids::JOURNAL_UNREADABLE),
                format!("{}@{K}:tx={TX2}", ids::JOURNAL_UNREADABLE)
            ],
            "two_journals_of_one_row_are_two_instances"
        );
        // A stuck journal's remediation is its forced set-aside (F2 s3).
        assert_eq!(
            two[0]["remediation"]["argv"],
            json!([
                "omaddons",
                "doctor",
                "fix",
                "--only",
                ids::JOURNAL_UNREADABLE,
                "-t",
                "retail",
                "--force",
                "--yes"
            ])
        );

        // A rename changes the label only.
        let renamed = |key: TargetKey| {
            if key.to_string() == K {
                "main".to_owned()
            } else {
                key.to_string()
            }
        };
        let input = JournalInput {
            journals: &journals(vec![diagnosis(JournalVerdict::Unreadable)]),
            lock: None,
            caller_holds_lock: false,
            target_id: &renamed,
        };
        let (id, obs) = journal_observations(&input).remove(0);
        let f: Finding = row(id).unwrap().finding(obs);
        assert_eq!(
            f.instance.to_string(),
            instances[0],
            "a_target_rename_changes_the_label_not_the_instance"
        );
        assert_eq!(f.label, format!("main · tx {TX}"));

        // An unregistered key: scoped by key, explained without -t.
        let other: TargetKey = "01M5ZQ7Y3KJ8V9W2X4C6B0N1RV".parse().unwrap();
        let unregistered = findings(vec![JournalDiagnosis {
            target: Some(other),
            ..diagnosis(JournalVerdict::TooNew)
        }]);
        let f = &unregistered[0];
        assert_eq!(
            f["instance"],
            format!("{}@{other}:tx={TX}", ids::JOURNAL_TOO_NEW)
        );
        let unscoped = json!(["omaddons", "doctor", "explain", ids::JOURNAL_TOO_NEW]);
        let forced = json!([
            "omaddons",
            "doctor",
            "fix",
            "--only",
            ids::JOURNAL_TOO_NEW,
            "--force",
            "--yes"
        ]);
        assert_eq!(
            (&f["remediation"]["argv"], &f["remediation"]["explain_argv"]),
            (&forced, &unscoped),
            "an_unregistered_key_is_scoped_by_key_and_explained_without_t"
        );

        let targetless = findings(vec![JournalDiagnosis {
            target: None,
            ..diagnosis(JournalVerdict::Unreadable)
        }]);
        assert_eq!(
            targetless[0]["instance"],
            format!("{}@machine:tx={TX}", ids::JOURNAL_UNREADABLE),
            "a_targetless_journal_is_machine_scoped"
        );
    }

    #[test]
    fn approximate_diagnoses_have_medium_confidence() {
        let approximate = JournalDiagnosis {
            evidence: JournalEvidence {
                approximations: vec![Approximation::PreservationWalk],
                ..JournalEvidence::default()
            },
            ..diagnosis(JournalVerdict::Unreadable)
        };
        assert_eq!(findings(vec![approximate])[0]["confidence"], "medium");
    }

    #[test]
    fn may_hold_only_copy_cases() {
        let tx: TxId = TX.parse().unwrap();
        for (case, counts, want) in [
            ("no_artifact", None, false),
            ("empty_stage", Some((0, 0)), false),
            ("non_empty_stage", Some((2, 0)), true),
            ("snapshot_with_addons", Some((0, 3)), true),
            ("wtf_previous_only", Some((0, 1)), true),
        ] {
            let mut j = journals(vec![diagnosis(JournalVerdict::Unreadable)]);
            if let Some((stage, snapshot)) = counts {
                j.counts.insert(
                    tx,
                    ArtifactCounts {
                        stage_folders: stage,
                        snapshot_folders: snapshot,
                        truncated: false,
                    },
                );
            }
            let got = findings_under(&j, None, false);
            assert_eq!(got[0]["evidence"]["may_hold_only_copy"], want, "{case}");
        }
    }

    #[test]
    fn a_truncated_count_makes_the_rows_incomplete() {
        let mut j = journals(vec![diagnosis(JournalVerdict::Unreadable)]);
        j.counts.insert(
            TX.parse().unwrap(),
            ArtifactCounts {
                stage_folders: 2,
                snapshot_folders: 0,
                truncated: true,
            },
        );
        let input = JournalInput {
            journals: &j,
            lock: None,
            caller_holds_lock: false,
            target_id: &retail,
        };
        assert!(journal_incomplete(&input, ids::JOURNAL_UNREADABLE).is_some());
        assert!(
            journal_incomplete(&input, ids::JOURNAL_TOO_NEW).is_none(),
            "only the row whose journal it is"
        );
    }

    #[test]
    fn plural_titles() {
        assert_eq!(
            plural_title(ids::JOURNAL_PENDING, 2).as_deref(),
            Some("2 interrupted changes wait to be finished")
        );
        assert_eq!(plural_title(ids::LOCK_HELD_LONG, 2), None, "one lock");
        for id in [
            ids::JOURNAL_PENDING,
            ids::JOURNAL_WAITS_FOR_GAME,
            ids::JOURNAL_TOO_NEW,
            ids::JOURNAL_UNREADABLE,
            ids::JOURNAL_UNEXPECTED_STATE,
            ids::JOURNAL_STEP_UNRECOGNIZED,
            ids::JOURNAL_MARKER_MISMATCH,
            ids::JOURNAL_UNATTRIBUTED,
            ids::JOURNAL_PRESERVATION_INCOMPLETE,
        ] {
            assert!(
                plural_title(id, 3).is_some_and(|t| t.starts_with("3 ")),
                "{id}"
            );
        }
    }

    #[test]
    fn lock_cases() {
        let now: Timestamp = "2026-10-31T12:00:00Z".parse().unwrap();
        let held = |secs: i64| LockState::Held {
            holder: Some(HolderInfo {
                pid: 4242,
                command: "omaddons update".to_owned(),
                started_at: now
                    .checked_sub(jiff::SignedDuration::from_secs(secs))
                    .unwrap(),
            }),
        };
        for (case, lock, own, age) in [
            ("free", LockState::Free, false, None),
            ("missing", LockState::Missing, false, None),
            (
                "held_without_record",
                LockState::Held { holder: None },
                false,
                None,
            ),
            ("held_9m59s", held(599), false, None),
            ("held_exactly_10m", held(600), false, None),
            ("held_10m01s", held(601), false, Some(601)),
            ("started_in_the_future", held(-60), false, None),
            ("caller_holds_it", held(3_600), true, None),
        ] {
            let got = lock_observation(&lock, own, now);
            assert_eq!(
                got.as_ref().and_then(|o| o.evidence.age_s),
                age,
                "{case}: {got:?}"
            );
        }
        let obs = lock_observation(&held(601), false, now).unwrap();
        let f: Finding = row(ids::LOCK_HELD_LONG).unwrap().finding(obs);
        assert_eq!(
            f.title,
            "An omaddons process (pid 4242, omaddons update) has held the lock for more than 10 minutes"
        );
        assert_eq!(
            f.instance.to_string(),
            format!("{}@machine", ids::LOCK_HELD_LONG)
        );
        assert_eq!(f.confidence, Confidence::Medium);
        assert_eq!(
            (f.evidence.pid, f.evidence.command.as_deref()),
            (Some(4242), Some("omaddons update"))
        );
        let prose = &f.remediation.command;
        assert!(f.remediation.argv.is_none(), "{prose}");
        assert!(prose.contains("kill 4242"), "{prose}");
        let words: Vec<&str> = prose.split_whitespace().collect();
        assert!(
            !words.windows(2).any(|pair| pair[0] == "omaddons"
                && crate::app::COMMANDS
                    .iter()
                    .any(|c| c.name.split_whitespace().next() == Some(pair[1]))),
            "{prose}"
        );
    }
}
