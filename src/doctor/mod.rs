//! The doctor (M4): the failure-mode registry every `status` issue and doctor finding
//! comes from, and the finding model. Pure: nothing here writes.

pub mod budget;
pub mod delegate;
mod detectors;
pub mod exit;
pub mod explain;
pub mod facts;
pub mod finding;
pub mod fix;
pub mod fix_runtime;
pub mod plan;
pub mod registry;
pub mod report;
pub mod runtime;
pub mod selection;
#[cfg(debug_assertions)]
pub mod test_fixers;
pub mod undo_script;
pub mod view;

pub use fix::plan_fix;

/// Every doctor failpoint (debug builds), checked against the source of `src/doctor/` and
/// of the repair transaction (`src/engine/repair.rs`, `src/engine/repair/`) by a test.
pub const FAILPOINTS: &[&str] = &[
    "doctor.run.staged",
    "doctor.run.published",
    "doctor.detector.panic",
    // `doctor fix`: planned, before the lock (the pause point of two concurrent fixers).
    "doctor.fix.planned",
    // The repair transaction (E5): after each journal line it writes, and after each
    // step a rollback inverts.
    "doctor.repair.prepared",
    "doctor.repair.step",
    "doctor.repair.done",
    "doctor.repair.history",
    "doctor.repair.committed",
    "doctor.recover.step",
];

use budget::Budget;
use finding::{Finding, Incomplete};
use registry::{FailureMode, Tier};
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
    detect_rows(registry::failure_modes(), view, selection, max_tier)
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
    use super::registry::FAILURE_MODES;
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

    /// A detector that reads nothing and finds one machine instance.
    fn one_machine_finding(_: &DoctorView, _: &Selection, _: &mut Budget) -> finding::Detected {
        finding::Detected {
            found: vec![finding::Observation {
                subject: finding::Subject::machine(),
                title: "t".to_owned(),
                confidence: finding::Confidence::High,
                addon: None,
                folder: None,
                evidence: finding::Evidence::default(),
            }],
            incomplete: None,
        }
    }

    #[test]
    fn failpoints_match_the_source() {
        use crate::source_scan::{Literals, production_code, sources};
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut found = std::collections::BTreeSet::new();
        let repair_root = root.join("src/engine/repair.rs");
        let repair_root = (
            repair_root.clone(),
            std::fs::read_to_string(&repair_root).unwrap(),
        );
        let scanned = sources(&root.join("src/doctor"))
            .into_iter()
            .chain(sources(&root.join("src/engine/repair")))
            .chain([repair_root]);
        for (_, text) in scanned {
            let code = production_code(&text, Literals::Keep).unwrap_or_default();
            for (at, _) in code.match_indices("failpoint::hit(\"doctor.") {
                let rest = &code[at + "failpoint::hit(\"".len()..];
                found.insert(rest.split('"').next().unwrap().to_owned());
            }
        }
        let listed: std::collections::BTreeSet<String> =
            FAILPOINTS.iter().map(|s| (*s).to_owned()).collect();
        assert_eq!(found, listed);
    }

    /// Checks take `&DoctorView` and never write (§6.12): no detector uses fsx's writer, as
    /// the view itself never does (`view_source_never_writes`). fsx's read-only types
    /// (`LockState`) are fine.
    #[test]
    fn detectors_never_write() {
        use crate::source_scan::{Literals, production_code, sources};
        const WRITES: [&str; 8] = [
            "WriteSession",
            "mutate(",
            "ensure_dir_all(",
            "prune(",
            "Op::",
            "OpenOptions",
            "fs::write",
            "lock_file",
        ];
        let writes =
            |code: &str| -> Vec<&str> { WRITES.into_iter().filter(|w| code.contains(w)).collect() };
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut found = Vec::new();
        for (path, text) in sources(&root.join("src/doctor/detectors")) {
            let code = production_code(&text, Literals::Blank).unwrap_or_default();
            for needle in writes(&code) {
                found.push(format!("{}: {needle}", path.display()));
            }
        }
        assert!(
            found.is_empty(),
            "detectors that write:\n{}",
            found.join("\n")
        );
        let planted = production_code("fn f() { crate::fsx::mutate(s, p, op); }", Literals::Blank);
        assert_eq!(
            writes(&planted.unwrap_or_default()),
            ["mutate("],
            "the scan sees one"
        );
    }

    #[test]
    fn status_rules_run_up_to_the_tier() {
        let cached = facts::findings(&facts(), Tier::Cached);
        let ids_of = |found: &[Finding]| {
            let mut ids: Vec<&str> = found.iter().map(|f| f.id).collect();
            ids.dedup();
            ids
        };
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
        let quick = facts::findings(&facts(), Tier::Quick);
        assert_eq!(ids_of(&quick).len(), 7, "{:?}", ids_of(&quick));
        assert!(ids_of(&quick).contains(&ids::MANAGED_FILES_MODIFIED));
    }

    #[test]
    fn detect_runs_selected_rows_up_to_the_tier() {
        let view = DoctorView::load_at(fixture_paths(), view::ViewOptions::default());
        let mut rows = FAILURE_MODES.to_vec();
        for row in &mut rows {
            row.detect = Some(one_machine_finding);
        }
        let all = Selection::all();
        let run = |sel: &Selection, tier| ids_of(&detect_rows(&rows, &view, sel, tier));
        let cached: Vec<&str> = rows
            .iter()
            .filter(|row| row.tier == Tier::Cached)
            .map(|row| row.id)
            .collect();
        assert_eq!(run(&all, Tier::Cached), cached);
        // Every row but the runtime's, which no tier selects.
        let detecting = rows.iter().filter(|row| row.tier != Tier::Runtime).count();
        assert_eq!(run(&all, Tier::Deep).len(), detecting);
        assert!(
            detecting < rows.len(),
            "the runtime's rows are in the registry"
        );
        assert_eq!(
            run(&Selection::only(&[ids::TARGET_VANISHED]), Tier::Deep),
            [ids::TARGET_VANISHED]
        );

        // A row without a detector is skipped.
        for row in &mut rows {
            row.detect = None;
        }
        assert_eq!(
            detect_rows(&rows, &view, &all, Tier::Deep),
            Detection::default()
        );
        assert_eq!(view.loaded_layers(), std::collections::BTreeSet::new());
    }

    #[test]
    fn findings_carry_their_rows_meta_and_keys() {
        let found = Detection {
            findings: facts::findings(&facts(), Tier::Quick),
            incomplete: Vec::new(),
        };
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
        let found = Detection {
            findings: facts::findings(
                &StatusFacts {
                    targets: vec![target],
                    machine: MachineFacts::default(),
                },
                Tier::Cached,
            ),
            incomplete: Vec::new(),
        };
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
