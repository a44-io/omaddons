//! The doctor (M4): the failure-mode registry every `status` issue and doctor finding
//! comes from, and the finding model. Pure: nothing here writes.

pub mod attribution;
pub mod budget;
pub mod delegate;
pub(crate) mod detectors;
pub mod exit;
pub mod explain;
pub mod finding;
pub mod fix;
pub mod fix_runtime;
pub mod fixers;
pub mod gc;
pub mod online;
pub mod plan;
pub mod registry;
pub mod report;
pub mod runs;
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
    // `doctor gc`: planned, before the lock (the pause point of the race tests).
    "doctor.gc.planned",
];

use crate::error::Result;
use finding::Finding;
use registry::{FailureMode, Tier};
use runtime::Budgets;
use selection::{Selection, Skipped};
use view::DoctorView;

/// What a run's detectors found, and the checks it could not make (never a pass).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Detection {
    pub findings: Vec<Finding>,
    pub skipped: Vec<Skipped>,
}

/// The registry's rows of `tiers` that `selection` includes, through the doctor's own
/// detector loop ([`runtime::detect`]), each under its tier's budget: a check that panics
/// is `detector-crashed`, one cut short `check-incomplete`, and one whose layers are
/// unavailable skipped (M4-D43). `status` runs it with the `cached` tier (`--cached`) or
/// the `cached` and `quick` tiers (M4-D29). Fails only when `-t` cannot be resolved. Pure:
/// reads only the view.
pub fn detect(view: &DoctorView, selection: &Selection, tiers: &[Tier]) -> Result<Detection> {
    detect_rows(view, selection, registry::failure_modes(), tiers)
}

/// [`detect`] over an explicit row slice: the testable core (synthetic rows).
#[doc(hidden)]
pub fn detect_rows(
    view: &DoctorView,
    selection: &Selection,
    rows: &[FailureMode],
    tiers: &[Tier],
) -> Result<Detection> {
    let diagnosis = runtime::detect(rows, view, selection, tiers, Budgets::TierDefaults)?;
    Ok(Detection {
        findings: diagnosis.findings,
        skipped: diagnosis.skipped,
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use std::path::PathBuf;

    use super::budget::Budget;
    use super::detectors::{AddonDriftFact, NewerFile, TargetFacts};
    use super::detectors::{external, plugins, schemas, state_files, userland};
    use super::registry::{FAILURE_MODES, Layer, ids};
    use super::selection::SkipReason;
    use super::view::{LayerState, Preloaded, Unavailable};
    use super::*;
    use crate::drift::DriftState;
    use crate::model::TargetKey;
    use crate::omarchy::status::WidgetInfo;
    use crate::state::paths::Paths;
    use crate::state::store::SchemaVersion;

    pub(crate) const K: &str = "01M5ZQ7Y3KJ8V9W2X4C6B0N1RT";
    const RECORD: &str = "/fixture/state/omaddons/targets/01M5ZQ7Y3KJ8V9W2X4C6B0N1RT.json";

    fn key() -> TargetKey {
        K.parse().unwrap()
    }

    /// Paths of a home that does not exist, for views whose layers are preloaded (or read
    /// nothing).
    pub(crate) fn fixture_paths() -> Paths {
        Paths::with_roots(
            "/fixture/home",
            "/fixture/config",
            "/fixture/state",
            "/fixture/cache",
        )
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

    /// A retail target holding every state the facts rules read.
    fn retail() -> TargetFacts {
        TargetFacts {
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
            ..TargetFacts::new("retail", key())
        }
    }

    fn ids_of(detection: &Detection) -> Vec<&'static str> {
        let mut ids: Vec<&str> = detection.findings.iter().map(|f| f.id).collect();
        ids.dedup();
        ids
    }

    fn observation() -> finding::Observation {
        finding::Observation {
            subject: finding::Subject::machine(),
            title: "t".to_owned(),
            confidence: finding::Confidence::High,
            addon: None,
            folder: None,
            evidence: finding::Evidence::default(),
        }
    }

    /// A detector that reads nothing and finds one machine instance.
    fn one_machine_finding(_: &DoctorView, _: &Selection, _: &mut Budget) -> finding::Detected {
        finding::Detected::complete(vec![observation()])
    }

    /// A detector that panics: the loop isolates it.
    fn panics(_: &DoctorView, _: &Selection, _: &mut Budget) -> finding::Detected {
        panic!("a check that fails"); // ubs:ignore — the panicking check the loop isolates
    }

    /// A detector its entry cap stops before it finds anything.
    fn cut_short(_: &DoctorView, _: &Selection, budget: &mut Budget) -> finding::Detected {
        let _ = budget.charge(u64::MAX);
        finding::Detected::cut_short(Vec::new(), budget)
    }

    /// The registry with every row given `detect` and no required layer, so a view of
    /// nothing runs them all.
    fn synthetic(detect: registry::DetectFn) -> Vec<FailureMode> {
        let mut rows = FAILURE_MODES.to_vec();
        for row in &mut rows {
            row.detect = Some(detect);
            row.requires = &[];
        }
        rows
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
    /// (`LockState`) are fine. Nor `BlobCache`, whose reads write `blobs/index.json`: the
    /// blob detector verifies through `blobs::verify_blob` alone (F6).
    #[test]
    fn detectors_never_write() {
        use crate::source_scan::{Literals, production_code, sources};
        const WRITES: [&str; 9] = [
            "WriteSession",
            "mutate(",
            "ensure_dir_all(",
            "prune(",
            "Op::",
            "OpenOptions",
            "fs::write",
            "lock_file",
            "BlobCache",
        ];
        let writes =
            |code: &str| -> Vec<&str> { WRITES.into_iter().filter(|w| code.contains(w)).collect() };
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut found = Vec::new();
        // Attribution (F2 s2) is the detectors' too: it reads what a set-aside would move.
        let attribution = root.join("src/doctor/attribution.rs");
        let scanned = sources(&root.join("src/doctor/detectors"))
            .into_iter()
            .chain([(
                attribution.clone(),
                std::fs::read_to_string(&attribution).unwrap(),
            )]);
        for (path, text) in scanned {
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

    /// `status --cached` runs the `cached` rows, `status` the `cached` and `quick` ones;
    /// never a `default`, `deep` or runtime row; `--only` narrows as in `doctor`.
    #[test]
    fn detect_runs_the_rows_of_its_tiers() {
        let view = DoctorView::load_at(fixture_paths(), view::ViewOptions::default());
        let rows = synthetic(one_machine_finding);
        let all = Selection::all();
        // Findings come merged and ranked (severity, id, …), so compare sorted ids.
        let run = |sel: &Selection, tiers: &[Tier]| {
            let mut ids = ids_of(&detect_rows(&view, sel, &rows, tiers).unwrap());
            ids.sort_unstable();
            ids
        };
        let of_tiers = |tiers: &[Tier]| -> Vec<&str> {
            let mut ids: Vec<&str> = rows
                .iter()
                .filter(|row| tiers.contains(&row.tier))
                .map(|row| row.id)
                .collect();
            ids.sort_unstable();
            ids
        };
        for (case, tiers) in [
            ("cached", &[Tier::Cached][..]),
            ("cached_and_quick", &[Tier::Cached, Tier::Quick][..]),
        ] {
            let ran = run(&all, tiers);
            assert_eq!(ran, of_tiers(tiers), "{case}");
            assert!(
                ran.iter()
                    .all(|id| registry::row(id).is_some_and(|row| tiers.contains(&row.tier))),
                "{case}: {ran:?} holds a row of another tier"
            );
        }
        assert!(
            !of_tiers(&[Tier::Quick]).is_empty(),
            "the registry has quick rows"
        );
        // No tier selects the runtime's own rows.
        let every = [Tier::Cached, Tier::Quick, Tier::Default, Tier::Deep];
        let detecting = rows.iter().filter(|row| row.tier != Tier::Runtime).count();
        assert_eq!(run(&all, &every).len(), detecting);
        assert!(
            detecting < rows.len(),
            "the runtime's rows are in the registry"
        );
        assert_eq!(
            run(&Selection::only(&[ids::TARGET_VANISHED]), &every),
            [ids::TARGET_VANISHED]
        );

        // A row without a detector runs nothing.
        let mut silent = rows.clone();
        for row in &mut silent {
            row.detect = None;
        }
        assert_eq!(
            detect_rows(&view, &all, &silent, &every).unwrap(),
            Detection::default()
        );
    }

    /// A row whose required layer is unavailable is skipped with that layer's cause, never
    /// run on defaults or reported clean.
    #[test]
    fn a_row_with_an_unavailable_layer_is_skipped_not_clean() {
        let view = DoctorView::preloaded(
            fixture_paths(),
            Preloaded {
                caches: Some(LayerState::Unavailable(Unavailable {
                    layer: Layer::Caches,
                    cause: Layer::Caches,
                })),
                ..Preloaded::default()
            },
        );
        let mut rows = synthetic(one_machine_finding);
        rows.retain(|row| row.id == ids::LOCK_HELD_LONG);
        rows[0].requires = &[Layer::Caches];
        let found = detect_rows(&view, &Selection::all(), &rows, &[Tier::Cached]).unwrap();
        assert_eq!(found.findings, Vec::new(), "never run");
        assert_eq!(
            found
                .skipped
                .iter()
                .map(|s| (s.id, s.reason))
                .collect::<Vec<_>>(),
            [(ids::LOCK_HELD_LONG, SkipReason::CachesUnavailable)]
        );
    }

    /// A panicking row is one `detector-crashed` finding naming it, and the next row runs.
    #[test]
    fn a_panicking_row_is_detector_crashed_and_the_loop_goes_on() {
        let view = DoctorView::load_at(fixture_paths(), view::ViewOptions::default());
        let mut rows = synthetic(one_machine_finding);
        rows.retain(|row| [ids::LOCK_HELD_LONG, ids::TARGET_VANISHED].contains(&row.id));
        rows[0].detect = Some(panics);
        let crashing = rows[0].id;
        let found = detect_rows(&view, &Selection::all(), &rows, &[Tier::Cached]).unwrap();
        let crashed: Vec<Option<&str>> = found
            .findings
            .iter()
            .filter(|f| f.id == ids::DETECTOR_CRASHED)
            .map(|f| f.evidence.detector)
            .collect();
        assert_eq!(crashed, [Some(crashing)], "{:?}", found.findings);
        assert!(
            found.findings.iter().any(|f| f.id == rows[1].id),
            "the second row ran: {:?}",
            found.findings
        );
        let _ = crate::cli::take_caught_panic();
    }

    /// A row a limit stopped short is `check-incomplete`, never a pass.
    #[test]
    fn an_incomplete_row_is_check_incomplete() {
        let view = DoctorView::load_at(fixture_paths(), view::ViewOptions::default());
        let mut rows = synthetic(cut_short);
        rows.retain(|row| row.id == ids::LOCK_HELD_LONG);
        let found = detect_rows(&view, &Selection::all(), &rows, &[Tier::Cached]).unwrap();
        let ids: Vec<(&str, Option<&str>)> = found
            .findings
            .iter()
            .map(|f| (f.id, f.evidence.detector))
            .collect();
        assert_eq!(ids, [(ids::CHECK_INCOMPLETE, Some(ids::LOCK_HELD_LONG))]);
    }

    /// `doctor`'s runtime and `status` share one loop: over rows without a fixer (whose
    /// remediation `diagnose` refines), the runtime's findings are `detect_rows`'.
    #[test]
    fn the_runtime_and_status_share_the_loop() {
        let view = DoctorView::load_at(fixture_paths(), view::ViewOptions::default());
        let mut rows = synthetic(one_machine_finding);
        rows.retain(|row| row.fix.is_none());
        rows[0].detect = Some(panics);
        let tiers = [Tier::Cached, Tier::Quick, Tier::Default];
        let shared = detect_rows(&view, &Selection::all(), &rows, &tiers).unwrap();
        let _ = crate::cli::take_caught_panic();
        let runtime = runtime::diagnose(
            &rows,
            &view,
            &Selection::all(),
            &tiers,
            Budgets::TierDefaults,
        )
        .unwrap();
        let _ = crate::cli::take_caught_panic();
        assert_eq!(runtime.findings, shared.findings);
        assert_eq!(runtime.skipped, shared.skipped);
        assert!(!shared.findings.is_empty());
    }

    /// The facts rules give findings with their rows' meta and stable instance keys, and
    /// content-free evidence.
    #[test]
    fn findings_carry_their_rows_meta_and_keys() {
        let targets = [retail()];
        let widget = WidgetInfo {
            omarchy: true,
            installed: true,
            enabled: true,
            version: Some("0.0.9".to_owned()),
            outdated: true,
            context_matches: Some(true),
        };
        let rules: [(&str, Vec<finding::Observation>); 6] = [
            (
                ids::MANAGED_FILES_MODIFIED,
                external::managed_files_modified_in(&targets),
            ),
            (
                ids::OWNERSHIP_TRANSFERRED,
                external::ownership_transferred_in(&targets),
            ),
            (
                ids::WIDGET_VERSION_DRIFT,
                plugins::widget_version_drift_in(Some(&widget)),
            ),
            (
                ids::STATE_VERSION_NEWER,
                schemas::state_version_newer_in(&targets),
            ),
            (
                ids::RECORD_FOLDER_MISSING,
                state_files::record_folder_missing_in(&targets),
            ),
            (ids::TARGET_VANISHED, userland::target_vanished_in(&targets)),
        ];
        let found: Vec<Finding> = rules
            .into_iter()
            .flat_map(|(id, observations)| {
                let row = registry::row(id).unwrap();
                observations.into_iter().map(|obs| row.finding(obs))
            })
            .collect();
        for f in &found {
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
            .iter()
            .map(|f| (f.instance.as_str(), f.label.as_str()))
            .collect();
        let expected = [
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
}
