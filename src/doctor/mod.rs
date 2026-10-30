//! The doctor (M4): the failure-mode registry every `status` issue and doctor finding
//! comes from, and the finding model. Pure: nothing here writes.

pub mod budget;
mod detectors;
pub mod facts;
pub mod finding;
pub mod registry;
pub mod selection;
pub mod view;

use budget::Budget;
use finding::{Finding, Incomplete};
use registry::{FAILURE_MODES, FailureMode, Tier};
use selection::Selection;
use view::DoctorView;

/// What a run's detectors found, and which of them a limit stopped short.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Detection {
    pub findings: Vec<Finding>,
    pub incomplete: Vec<(&'static str, Incomplete)>,
}

/// Runs every selected row up to `max_tier`, in registry order, and turns what each
/// detector found into findings. Pure: reads only the view. (Budgets and isolating a
/// detector that panics are E3's.)
pub fn detect(view: &DoctorView, selection: &Selection, max_tier: Tier) -> Detection {
    detect_rows(FAILURE_MODES, view, selection, max_tier)
}

fn detect_rows(
    rows: &[FailureMode],
    view: &DoctorView,
    selection: &Selection,
    max_tier: Tier,
) -> Detection {
    let mut out = Detection::default();
    for row in rows {
        if !selection.includes(row) || row.tier > max_tier {
            continue;
        }
        let Some(detect) = row.detect else {
            continue;
        };
        let detected = detect(view, selection, &mut Budget::unlimited());
        out.findings
            .extend(detected.found.into_iter().map(|obs| row.finding(obs)));
        if let Some(incomplete) = detected.incomplete {
            out.incomplete.push((row.id, incomplete));
        }
    }
    out
}

#[cfg(test)]
pub(crate) mod tests {
    use std::path::PathBuf;

    use super::facts::{
        AddonDriftFact, MachineFacts, NewerFile, PendingJournal, StatusFacts, TargetFacts,
        WidgetDrift,
    };
    use super::registry::ids;
    use super::*;
    use crate::drift::DriftState;
    use crate::engine::journal::Unrecoverable;
    use crate::model::{TargetKey, TxId};
    use crate::state::paths::Paths;
    use crate::state::store::SchemaVersion;

    pub(crate) const K: &str = "01M5ZQ7Y3KJ8V9W2X4C6B0N1RT";
    pub(crate) const T: &str = "01M7A1B2C3D4E5F6G7H8J9K0MN";
    const T2: &str = "01M7A1B2C3D4E5F6G7H8J9K0MP";
    const RECORD: &str = "/fixture/state/omaddons/targets/01M5ZQ7Y3KJ8V9W2X4C6B0N1RT.json";

    fn key() -> TargetKey {
        K.parse().unwrap()
    }

    /// Paths of a home that does not exist, for views built from facts (which read none).
    pub(crate) fn fixture_paths() -> Paths {
        Paths::with_roots(
            "/fixture/home",
            "/fixture/config",
            "/fixture/state",
            "/fixture/cache",
        )
    }

    fn tx(text: &str) -> TxId {
        text.parse().unwrap()
    }

    fn journal(text: &str, unrecoverable: Option<Unrecoverable>) -> PendingJournal {
        PendingJournal {
            tx: tx(text),
            unrecoverable,
        }
    }

    fn drift(addon: &str, state: DriftState, files: u64) -> AddonDriftFact {
        AddonDriftFact {
            addon: addon.to_owned(),
            state,
            files,
            missing_folders: if state == DriftState::Missing {
                vec![PathBuf::from(format!(
                    "/fixture/client/Interface/AddOns/{addon}"
                ))]
            } else {
                Vec::new()
            },
        }
    }

    /// A retail target with nothing wrong.
    pub(crate) fn retail() -> TargetFacts {
        TargetFacts {
            id: "retail".to_owned(),
            key: key(),
            journals: Vec::new(),
            newer_record: None,
            vanished: None,
            degraded: 0,
            drift: Vec::new(),
        }
    }

    /// Facts holding every one of the seven states.
    fn facts() -> StatusFacts {
        let target = TargetFacts {
            journals: vec![journal(T, None)],
            newer_record: Some(NewerFile {
                path: RECORD.into(),
                on_disk: SchemaVersion::new(2, 0),
                writes: SchemaVersion::new(1, 0),
            }),
            vanished: Some("/fixture/client".into()),
            degraded: 2,
            drift: vec![
                drift("Details", DriftState::Missing, 0),
                drift("Plater", DriftState::Modified, 3),
            ],
            ..retail()
        };
        StatusFacts {
            targets: vec![target],
            machine: MachineFacts {
                unattributed: vec![journal(T2, Some(Unrecoverable::Unreadable))],
                widget: Some(WidgetDrift {
                    version: Some("0.0.9".to_owned()),
                }),
            },
        }
    }

    fn ids_of(detection: &Detection) -> Vec<&'static str> {
        let mut ids: Vec<&str> = detection.findings.iter().map(|f| f.id).collect();
        ids.dedup();
        ids
    }

    #[test]
    fn detect_runs_selected_rows_up_to_the_tier() {
        let view = DoctorView::from_status_facts(fixture_paths(), facts());
        let all = Selection::all();
        let cached = detect(&view, &all, Tier::Cached);
        assert_eq!(
            ids_of(&cached),
            [
                ids::JOURNAL_PENDING,
                ids::OWNERSHIP_TRANSFERRED,
                ids::WIDGET_VERSION_DRIFT,
                ids::STATE_VERSION_NEWER,
                ids::RECORD_FOLDER_MISSING,
                ids::TARGET_VANISHED,
            ],
            "cached runs no managed-files-modified"
        );
        let quick = detect(&view, &all, Tier::Quick);
        assert_eq!(ids_of(&quick).len(), 7, "{:?}", ids_of(&quick));
        assert!(ids_of(&quick).contains(&ids::MANAGED_FILES_MODIFIED));
        let only = detect(&view, &Selection::only(&[ids::TARGET_VANISHED]), Tier::Deep);
        assert_eq!(ids_of(&only), [ids::TARGET_VANISHED]);
        assert!(quick.incomplete.is_empty());

        // A row without a detector is skipped.
        let mut rows = FAILURE_MODES.to_vec();
        for row in &mut rows {
            row.detect = None;
        }
        assert_eq!(
            detect_rows(&rows, &view, &all, Tier::Deep),
            Detection::default()
        );
    }

    #[test]
    fn findings_carry_their_rows_meta_and_keys() {
        let view = DoctorView::from_status_facts(fixture_paths(), facts());
        let found = detect(&view, &Selection::all(), Tier::Quick);
        for f in &found.findings {
            let row = registry::row(f.id).unwrap();
            assert_eq!(
                (f.severity, f.subsystem),
                (row.severity, row.subsystem),
                "{}",
                f.id
            );
            assert!(f.evidence.is_private_safe(), "{}: {:?}", f.id, f.evidence);
            assert_eq!(f.confidence, finding::Confidence::High, "{}", f.id);
        }
        let keys: Vec<(&str, &str)> = found
            .findings
            .iter()
            .map(|f| (f.instance.as_str(), f.label.as_str()))
            .collect();
        let expected = [
            (
                format!("{}@{K}:tx={T}", ids::JOURNAL_PENDING),
                format!("retail · tx {T}"),
            ),
            (
                format!("{}@machine:tx={T2}", ids::JOURNAL_PENDING),
                format!("machine · tx {T2}"),
            ),
            (
                format!("{}@{K}:addon=plater", ids::MANAGED_FILES_MODIFIED),
                "retail/Plater".to_owned(),
            ),
            (
                format!("{}@{K}", ids::OWNERSHIP_TRANSFERRED),
                "retail".to_owned(),
            ),
            (
                format!("{}@machine", ids::WIDGET_VERSION_DRIFT),
                "machine".to_owned(),
            ),
            (
                format!("{}@{K}:path={RECORD}", ids::STATE_VERSION_NEWER),
                format!("retail · {RECORD}"),
            ),
            (
                format!("{}@{K}:addon=details", ids::RECORD_FOLDER_MISSING),
                "retail/Details".to_owned(),
            ),
            (format!("{}@{K}", ids::TARGET_VANISHED), "retail".to_owned()),
        ];
        let expected: Vec<(&str, &str)> = expected
            .iter()
            .map(|(instance, label)| (instance.as_str(), label.as_str()))
            .collect();
        assert_eq!(keys, expected);
    }

    #[test]
    fn two_journals_are_two_findings() {
        let target = TargetFacts {
            journals: vec![
                journal(T, None),
                journal(T2, Some(Unrecoverable::Unreadable)),
            ],
            ..retail()
        };
        let view = DoctorView::from_status_facts(
            fixture_paths(),
            StatusFacts {
                targets: vec![target],
                machine: MachineFacts::default(),
            },
        );
        let found = detect(&view, &Selection::all(), Tier::Cached);
        let journals: Vec<(&str, &str)> = found
            .findings
            .iter()
            .map(|f| (f.instance.as_str(), f.title.as_str()))
            .collect();
        assert_eq!(journals.len(), 2, "{journals:?}");
        assert_ne!(journals[0].0, journals[1].0);
        assert_eq!(journals[0].1, "An interrupted change waits to be finished");
        assert_eq!(journals[1].1, "An interrupted change needs manual repair");
        assert_eq!(
            found.findings[1].evidence.reason,
            Some("journal_unreadable")
        );
    }
}
