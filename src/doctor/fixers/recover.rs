//! The delegation fixer (M4-D37), of `journal-pending` and `journal-preservation-incomplete`
//! (Appendix B rows 1 and 9): a journal recovery can settle is settled by the engine's own
//! recovery, which `doctor fix` runs for exactly the journals its preview disclosed
//! (`doctor::delegate`). This plan only says which: one `Fix::Delegate` per instance whose
//! diagnosis recovery would settle, and none for one it would not (a live writer, a
//! predicted stop, a journal behind a barrier, a committed one).

use crate::doctor::finding::Finding;
use crate::doctor::registry::Refusal;
use crate::doctor::view::DoctorView;
use crate::engine::journal::Phase;
use crate::engine::repair::RepairPhase;
use crate::engine::repair_action::{Delegation, Direction, Fix, JournalFamily, JournalPhase};
use crate::engine::simulate::{self, JournalDiagnosis, JournalVerdict};

/// The fix of one finding: its transaction's delegation, or nothing to do when recovery
/// would not settle it now (E6 counts such an instance among the unfixable ones).
pub fn plan(view: &DoctorView, finding: &Finding) -> Result<Vec<Fix>, Refusal> {
    let Some(tx) = finding.evidence.tx else {
        return Ok(Vec::new());
    };
    let Some(journals) = view.journals() else {
        return Ok(Vec::new());
    };
    Ok(journals
        .diagnoses
        .journals
        .iter()
        .find(|d| d.tx == tx)
        .and_then(delegation_for)
        .map(|what| Fix::Delegate { what })
        .into_iter()
        .collect())
}

/// What a delegated recovery of `d` discloses, as `journal::diagnose` predicts it: its
/// family, the phase it stands in and which way it settles. `None` when recovery would not
/// settle it: its writer is alive, it is committed, it stops, waits for its game, waits
/// behind a barrier, or cannot be read.
pub fn delegation_for(d: &JournalDiagnosis) -> Option<Delegation> {
    if d.live {
        return None;
    }
    let repair = d.family == JournalFamily::Repair;
    let (phase, direction) = match d.verdict {
        JournalVerdict::Recoverable { direction, .. } => match direction {
            // Never prepared: recovery only removes it, a rollback of nothing. Committed
            // (or a repair rolled back in process): nothing to disclose.
            simulate::Direction::Prune if d.phase == Phase::Unprepared => {
                (JournalPhase::Unprepared, Direction::RollBack)
            }
            simulate::Direction::Prune => return None,
            simulate::Direction::Back if repair => {
                let phase = match d.repair_phase {
                    Some(RepairPhase::Stepped(_)) => JournalPhase::Step,
                    _ => JournalPhase::Prepared,
                };
                (phase, Direction::RollBack)
            }
            simulate::Direction::Back => (JournalPhase::Prepared, Direction::RollBack),
            simulate::Direction::Forward if repair => (JournalPhase::Done, Direction::RollForward),
            simulate::Direction::Forward => (JournalPhase::Swapped, Direction::RollForward),
        },
        // A preservation recovery resumes, then rolls the restore back.
        JournalVerdict::PreservationIncomplete { stops: false } => {
            (JournalPhase::Preserve, Direction::RollBack)
        }
        _ => return None,
    };
    Some(Delegation {
        tx: d.tx,
        family: d.family,
        phase,
        direction,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use super::*;
    use crate::doctor::finding::{Confidence, Evidence, Observation, Subject};
    use crate::doctor::registry::{self, ids};
    use crate::doctor::view::{Journals, LayerState, Preloaded};
    use crate::engine::journal::StopReason;
    use crate::engine::simulate::{
        Diagnoses, JournalEvidence, OwnerStop, ProtectedPaths, UnattributedCause,
    };
    use crate::error::ErrorCode;
    use crate::model::{TargetKey, TxId};
    use crate::state::paths::Paths;

    const TX: &str = "01JB8Q2VA00000000000000009";

    fn tx() -> TxId {
        TX.parse().unwrap()
    }

    fn diagnosis(family: JournalFamily, phase: Phase, verdict: JournalVerdict) -> JournalDiagnosis {
        JournalDiagnosis {
            tx: tx(),
            family,
            path: PathBuf::from(format!("/state/omaddons/journal/{TX}.jsonl")),
            target: Some("01JB8Q2VA00000000000000002".parse::<TargetKey>().unwrap()),
            client_dir: Some(PathBuf::from("/wow/_retail_")),
            phase,
            repair_phase: None,
            live: false,
            verdict,
            evidence: JournalEvidence::default(),
            protected: ProtectedPaths::default(),
        }
    }

    fn recoverable(phase: Phase, direction: simulate::Direction) -> JournalVerdict {
        JournalVerdict::Recoverable { phase, direction }
    }

    fn delegation(family: JournalFamily, phase: JournalPhase, direction: Direction) -> Delegation {
        Delegation {
            tx: tx(),
            family,
            phase,
            direction,
        }
    }

    /// One case per row of F2 s1's table (§2).
    #[test]
    fn delegation_for_each_prediction() {
        use JournalFamily::{Folder, Repair};
        use simulate::Direction::{Back, Forward, Prune};
        let repair_at = |phase: RepairPhase, folder: Phase, verdict| JournalDiagnosis {
            repair_phase: Some(phase),
            ..diagnosis(Repair, folder, verdict)
        };
        let stop = OwnerStop {
            code: ErrorCode::Internal,
            reason: StopReason::StepUnrecognized,
        };
        let cases: Vec<(&str, JournalDiagnosis, Option<Delegation>)> = vec![
            (
                "folder_prepared_rolls_back",
                diagnosis(Folder, Phase::Prepared, recoverable(Phase::Prepared, Back)),
                Some(delegation(
                    Folder,
                    JournalPhase::Prepared,
                    Direction::RollBack,
                )),
            ),
            (
                "folder_swapped_rolls_forward",
                diagnosis(Folder, Phase::Swapped, recoverable(Phase::Swapped, Forward)),
                Some(delegation(
                    Folder,
                    JournalPhase::Swapped,
                    Direction::RollForward,
                )),
            ),
            (
                "unprepared_is_a_rollback_of_nothing",
                diagnosis(
                    Folder,
                    Phase::Unprepared,
                    recoverable(Phase::Unprepared, Prune),
                ),
                Some(delegation(
                    Folder,
                    JournalPhase::Unprepared,
                    Direction::RollBack,
                )),
            ),
            (
                "unprepared_repair_is_a_rollback_of_nothing",
                repair_at(
                    RepairPhase::Unprepared,
                    Phase::Unprepared,
                    recoverable(Phase::Unprepared, Prune),
                ),
                Some(delegation(
                    Repair,
                    JournalPhase::Unprepared,
                    Direction::RollBack,
                )),
            ),
            (
                "resumable_preservation",
                diagnosis(
                    Folder,
                    Phase::Prepared,
                    JournalVerdict::PreservationIncomplete { stops: false },
                ),
                Some(delegation(
                    Folder,
                    JournalPhase::Preserve,
                    Direction::RollBack,
                )),
            ),
            (
                "repair_before_done",
                repair_at(
                    RepairPhase::Prepared,
                    Phase::Prepared,
                    recoverable(Phase::Prepared, Back),
                ),
                Some(delegation(
                    Repair,
                    JournalPhase::Prepared,
                    Direction::RollBack,
                )),
            ),
            (
                "repair_after_a_step",
                repair_at(
                    RepairPhase::Stepped(2),
                    Phase::Prepared,
                    recoverable(Phase::Prepared, Back),
                ),
                Some(delegation(Repair, JournalPhase::Step, Direction::RollBack)),
            ),
            (
                "repair_at_done",
                repair_at(
                    RepairPhase::Done,
                    Phase::Swapped,
                    recoverable(Phase::Swapped, Forward),
                ),
                Some(delegation(
                    Repair,
                    JournalPhase::Done,
                    Direction::RollForward,
                )),
            ),
            (
                "live_writer_none",
                JournalDiagnosis {
                    live: true,
                    ..diagnosis(Folder, Phase::Prepared, recoverable(Phase::Prepared, Back))
                },
                None,
            ),
            (
                "predicted_stop_none",
                diagnosis(
                    Folder,
                    Phase::Prepared,
                    JournalVerdict::StepUnrecognized { step: 2 },
                ),
                None,
            ),
            (
                "preservation_that_stops_none",
                diagnosis(
                    Folder,
                    Phase::Prepared,
                    JournalVerdict::PreservationIncomplete { stops: true },
                ),
                None,
            ),
            (
                "waits_for_game_none",
                diagnosis(Folder, Phase::Prepared, JournalVerdict::WaitsForGame),
                None,
            ),
            (
                "behind_barrier_none",
                diagnosis(
                    Folder,
                    Phase::Prepared,
                    JournalVerdict::BehindBarrier {
                        owner: "01JB8Q2VA0000000000000000A".parse().unwrap(),
                        stop: Some(stop),
                    },
                ),
                None,
            ),
            (
                "too_new_none",
                diagnosis(Folder, Phase::Prepared, JournalVerdict::TooNew),
                None,
            ),
            (
                "unattributed_none",
                diagnosis(
                    Folder,
                    Phase::Prepared,
                    JournalVerdict::Unattributed {
                        cause: UnattributedCause::Unreadable,
                    },
                ),
                None,
            ),
            (
                "committed_none",
                diagnosis(
                    Folder,
                    Phase::Committed,
                    recoverable(Phase::Committed, Prune),
                ),
                None,
            ),
            (
                "repair_rolled_back_none",
                repair_at(
                    RepairPhase::RolledBack,
                    Phase::Committed,
                    recoverable(Phase::Committed, Prune),
                ),
                None,
            ),
        ];
        for (case, d, want) in cases {
            assert_eq!(delegation_for(&d), want, "{case}: {:?}", d.verdict);
        }
    }

    /// A view whose Journals layer holds `diagnoses`.
    fn view_of(diagnoses: Vec<JournalDiagnosis>) -> (tempfile::TempDir, DoctorView) {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::with_roots(
            dir.path(),
            dir.path().join("c"),
            dir.path().join("s"),
            dir.path().join("k"),
        );
        let journals = Journals {
            pending: Vec::new(),
            diagnoses: Diagnoses {
                journals: diagnoses,
                unlisted: None,
            },
            counts: BTreeMap::new(),
        };
        let view = DoctorView::preloaded(
            paths,
            Preloaded {
                journals: Some(LayerState::Available(journals)),
                ..Preloaded::default()
            },
        );
        (dir, view)
    }

    /// The row-1 finding of transaction `tx`.
    fn pending_finding(tx: Option<TxId>) -> Finding {
        let row = registry::row(ids::JOURNAL_PENDING).unwrap();
        row.finding(Observation {
            subject: Subject::machine(),
            title: "An interrupted change waits to be finished".to_owned(),
            confidence: Confidence::High,
            addon: None,
            folder: None,
            evidence: Evidence {
                tx,
                ..Evidence::default()
            },
        })
    }

    #[test]
    fn plan_delegates_one_per_instance() {
        let (_dir, view) = view_of(vec![diagnosis(
            JournalFamily::Folder,
            Phase::Swapped,
            recoverable(Phase::Swapped, simulate::Direction::Forward),
        )]);
        let fixes = plan(&view, &pending_finding(Some(tx()))).unwrap();
        assert_eq!(
            fixes,
            [Fix::Delegate {
                what: delegation(
                    JournalFamily::Folder,
                    JournalPhase::Swapped,
                    Direction::RollForward
                )
            }]
        );
        assert_eq!(
            plan(&view, &pending_finding(Some(TxId::new()))).unwrap(),
            [],
            "an instance whose journal the view does not list"
        );
        assert_eq!(plan(&view, &pending_finding(None)).unwrap(), [], "no tx");
    }

    #[test]
    fn plan_is_empty_for_a_predicted_stop() {
        let (_dir, view) = view_of(vec![diagnosis(
            JournalFamily::Folder,
            Phase::Prepared,
            JournalVerdict::PreservationIncomplete { stops: true },
        )]);
        assert_eq!(plan(&view, &pending_finding(Some(tx()))).unwrap(), []);
    }

    #[test]
    fn plan_is_empty_for_a_live_writer() {
        let (_dir, view) = view_of(vec![JournalDiagnosis {
            live: true,
            ..diagnosis(
                JournalFamily::Folder,
                Phase::Prepared,
                recoverable(Phase::Prepared, simulate::Direction::Back),
            )
        }]);
        assert_eq!(plan(&view, &pending_finding(Some(tx()))).unwrap(), []);
    }
}
