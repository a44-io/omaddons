//! The set-aside (M4-D38), of the six stuck journal states (Appendix B rows 3-8): a
//! forced repair that suspends a stuck journal's set into quarantine, which unblocks its
//! client and keeps every byte, recording guards so `doctor undo` puts it back only while
//! nothing it depends on has changed. What it moves is attribution's (`doctor::
//! attribution`): found by the transaction id alone, never by a path the journal names.
//! This plan only says what to move; E5 lowers it, E6 gates and runs it.

use std::path::{Path, PathBuf};

use crate::discover::registry::same_dir;
use crate::discover::running::{RunningScan, detect_running};
use crate::doctor::attribution::{attribute, manifest};
use crate::doctor::budget::Budget;
use crate::doctor::finding::Finding;
use crate::doctor::registry::{Refusal, RefusalReason};
use crate::doctor::view::DoctorView;
use crate::engine::repair_action::{Fix, Guard, JournalSet, QuarantineRoot, SuspendedArtifact};
use crate::limits::{DOCTOR_DETECTOR_DEADLINE, DOCTOR_SCAN_MAX_ENTRIES};
use crate::model::Target;

/// The set-aside of one stuck journal's finding: one `Fix::Suspend` of its attributed set,
/// in move order (client stages, client snapshots, state snapshots, the in-flight lock,
/// the journal last), with its guards. Nothing when its writer is alive, when nothing can
/// be attributed, or when a guard cannot be recorded (nothing is set aside without them).
pub fn plan(view: &DoctorView, finding: &Finding) -> Result<Vec<Fix>, Refusal> {
    plan_with(view, finding, &detect_running)
}

/// [`plan`] with the process table read by `scan` (the unit tests' seam).
pub fn plan_with(
    view: &DoctorView,
    finding: &Finding,
    scan: &dyn Fn(&[Target]) -> RunningScan,
) -> Result<Vec<Fix>, Refusal> {
    let Some(tx) = finding.evidence.tx else {
        return Ok(Vec::new());
    };
    let Some(d) = view
        .journals()
        .and_then(|j| j.diagnoses.journals.iter().find(|d| d.tx == tx))
    else {
        return Ok(Vec::new());
    };
    // Its writer is alive: the journal is not stuck yet.
    if d.live {
        return Ok(Vec::new());
    }
    let mut budget = Budget::new(DOCTOR_DETECTOR_DEADLINE, DOCTOR_SCAN_MAX_ENTRIES);
    let a = attribute(view, d, &mut budget);
    if a.artifacts.is_empty() {
        return Ok(Vec::new());
    }
    let Ok(guards) = manifest(view, d, &a) else {
        return Ok(Vec::new());
    };

    // Every client the set moves from or a guard reads: none may run, or may.
    let mut involved: Vec<PathBuf> = a.clients.iter().map(|(dir, _)| dir.clone()).collect();
    let registered: Vec<(PathBuf, crate::model::TargetKey)> = view
        .registry()
        .map(|r| {
            r.value
                .targets
                .iter()
                .map(|e| (e.client_dir.clone(), e.key))
                .collect()
        })
        .unwrap_or_default();
    for g in &guards {
        let dir = match g {
            Guard::Marker { target, .. } | Guard::WtfEpoch { target, .. } => registered
                .iter()
                .find(|(_, k)| k == target)
                .map(|(dir, _)| dir.clone()),
            Guard::Epoch { path, .. } => registered
                .iter()
                .find(|(dir, _)| path.starts_with(dir))
                .map(|(dir, _)| dir.clone()),
            Guard::Generation { .. } => None,
        };
        involved.extend(dir);
    }
    involved.sort();
    involved.dedup();
    if !involved.is_empty() {
        refuse_running(view, &involved, tx, scan)?;
    }

    let artifacts = a
        .artifacts
        .iter()
        .map(|artifact| SuspendedArtifact {
            path: artifact.path.clone(),
            root: match &artifact.client {
                Some(client_dir) => QuarantineRoot::Client {
                    client_dir: client_dir.clone(),
                },
                None => QuarantineRoot::State,
            },
        })
        .collect();
    Ok(vec![Fix::Suspend {
        journal: JournalSet {
            tx,
            family: d.family,
            target: d.target,
            artifacts,
            guards,
            attribution: a.attribution,
            unattributed: a.unattributed,
        },
    }])
}

/// `client_running` when the game of a client among `involved` runs, or may: the process
/// table could not be read (M2's fail-closed rule).
fn refuse_running(
    view: &DoctorView,
    involved: &[PathBuf],
    tx: crate::model::TxId,
    scan: &dyn Fn(&[Target]) -> RunningScan,
) -> Result<(), Refusal> {
    let discovered: Vec<Target> = view
        .world()
        .map(|w| {
            w.resolved
                .targets
                .iter()
                .map(|d| d.target.clone())
                .filter(|t| involved.iter().any(|dir| same_dir(&t.client_dir, dir)))
                .collect()
        })
        .unwrap_or_default();
    let found = scan(&discovered);
    let id_of = |dir: &Path| {
        discovered
            .iter()
            .find(|t| same_dir(&t.client_dir, dir))
            .map_or_else(|| dir.display().to_string(), |t| t.id.clone())
    };
    let running = if found.unavailable.is_some() {
        involved.first().map(|dir| id_of(dir))
    } else {
        involved
            .iter()
            .find(|dir| found.clients.keys().any(|running| same_dir(running, dir)))
            .map(|dir| id_of(dir))
    };
    match running {
        Some(id) => Err(Refusal {
            reason: RefusalReason::ClientRunning,
            message: format!(
                "{id}'s game is running, or may be; quit it before setting aside transaction {tx}"
            ),
        }),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::doctor::attribution::tests::{
        Home, both, diagnosis, home, mkdir, plant_retail_set, retail_set, step_line, tx, view_with,
        write,
    };
    use crate::doctor::finding::{Confidence, Evidence, Observation, Subject};
    use crate::doctor::registry::{self, ids};
    use crate::engine::repair_action::{Attribution, JournalFamily, Primitive};
    use crate::engine::simulate::{JournalDiagnosis, JournalVerdict, UnattributedCause};
    use crate::model::TargetKey;
    use crate::state::paths::ClientPaths;

    /// Row `id`'s finding of the transaction, on retail (`Some`) or the machine.
    fn finding(id: &str, retail: Option<TargetKey>) -> Finding {
        let subject = match retail {
            Some(key) => Subject::target("retail", key),
            None => Subject::machine(),
        };
        registry::row(id).unwrap().finding(Observation {
            subject: subject.tx(tx()),
            title: "a stuck journal".to_owned(),
            confidence: Confidence::High,
            addon: None,
            folder: None,
            evidence: Evidence {
                tx: Some(tx()),
                ..Evidence::default()
            },
        })
    }

    fn idle(_: &[Target]) -> RunningScan {
        RunningScan::default()
    }

    fn running_at(dir: &Path) -> RunningScan {
        RunningScan {
            clients: BTreeMap::from([(dir.to_path_buf(), Vec::new())]),
            unavailable: None,
        }
    }

    /// The retail set, planted, and its diagnosis with `verdict`.
    fn retail_journal(h: &Home, verdict: JournalVerdict) -> JournalDiagnosis {
        let addons = ClientPaths::new(&h.retail).addons();
        let stage = ClientPaths::new(&h.retail).stage(&tx());
        plant_retail_set(h, &step_line(1, &addons.join("Foo"), &stage.join("Foo")));
        diagnosis(
            h.paths.journal(&tx()),
            JournalFamily::Folder,
            Some(h.rk),
            verdict,
        )
    }

    fn suspended(fixes: &[Fix]) -> &JournalSet {
        match fixes {
            [Fix::Suspend { journal }] => journal,
            other => panic!("one suspend: {other:#?}"), // ubs:ignore — fails the test
        }
    }

    /// The set as `(path, root)` pairs.
    fn set_of(journal: &JournalSet) -> Vec<(PathBuf, QuarantineRoot)> {
        journal
            .artifacts
            .iter()
            .map(|a| (a.path.clone(), a.root.clone()))
            .collect()
    }

    /// The retail set's `(path, root)` pairs, in move order.
    fn retail_roots(h: &Home) -> Vec<(PathBuf, QuarantineRoot)> {
        retail_set(h)
            .into_iter()
            .map(|(kind, path)| {
                let root = match kind {
                    crate::doctor::attribution::ArtifactKind::ClientStage
                    | crate::doctor::attribution::ArtifactKind::ClientSnapshot => {
                        QuarantineRoot::Client {
                            client_dir: h.retail.clone(),
                        }
                    }
                    _ => QuarantineRoot::State,
                };
                (path, root)
            })
            .collect()
    }

    #[test]
    fn plan_builds_one_suspend_per_instance() {
        // A trusted journal: the whole retail set, the journal last, with its guards.
        let h = home();
        let d = retail_journal(&h, JournalVerdict::StepUnrecognized { step: 1 });
        let view = view_with(&h, Some(both(&h)), Some(vec![d.clone()]));
        let fixes = plan_with(
            &view,
            &finding(ids::JOURNAL_STEP_UNRECOGNIZED, Some(h.rk)),
            &idle,
        )
        .unwrap();
        let journal = suspended(&fixes);
        assert_eq!(
            set_of(journal),
            retail_roots(&h),
            "trusted_journal: {journal:#?}"
        );
        assert_eq!(
            (journal.tx, journal.family, journal.target),
            (tx(), JournalFamily::Folder, Some(h.rk))
        );
        assert_eq!(journal.attribution, Attribution::Complete);
        let a = attribute(&view, &d, &mut Budget::unlimited());
        assert_eq!(
            journal.guards,
            manifest(&view, &d, &a).unwrap(),
            "trusted_journal"
        );
        assert!(
            journal
                .guards
                .iter()
                .any(|g| matches!(g, Guard::Epoch { .. })),
            "its folder's epoch"
        );

        // A newer journal: the same set, no folder epochs.
        let h = home();
        let d = retail_journal(&h, JournalVerdict::TooNew);
        let view = view_with(&h, Some(both(&h)), Some(vec![d]));
        let fixes = plan_with(&view, &finding(ids::JOURNAL_TOO_NEW, Some(h.rk)), &idle).unwrap();
        let journal = suspended(&fixes);
        assert_eq!(set_of(journal), retail_roots(&h), "newer_journal");
        assert!(
            journal
                .guards
                .iter()
                .all(|g| !matches!(g, Guard::Epoch { .. })),
            "newer_journal: {journal:#?}"
        );

        // Artifacts on two clients: both clients' stages, each into its own root.
        let h = home();
        let d = retail_journal(&h, JournalVerdict::StepUnrecognized { step: 1 });
        mkdir(&ClientPaths::new(&h.era).stage(&tx()));
        let view = view_with(&h, Some(both(&h)), Some(vec![d]));
        let fixes = plan_with(
            &view,
            &finding(ids::JOURNAL_STEP_UNRECOGNIZED, Some(h.rk)),
            &idle,
        )
        .unwrap();
        let journal = suspended(&fixes);
        let era_stage = (
            ClientPaths::new(&h.era).stage(&tx()),
            QuarantineRoot::Client {
                client_dir: h.era.clone(),
            },
        );
        assert!(
            set_of(journal).contains(&era_stage),
            "two_clients: {journal:#?}"
        );
        let marked: Vec<TargetKey> = journal
            .guards
            .iter()
            .filter_map(|g| match g {
                Guard::Marker { target, .. } => Some(*target),
                _ => None,
            })
            .collect();
        assert!(
            marked.contains(&h.rk) && marked.contains(&h.ek),
            "two_clients: {marked:?}"
        );
        assert!(matches!(journal.artifacts.last(), Some(a) if a.path == h.paths.journal(&tx())));
    }

    #[test]
    fn plan_refuses_while_an_involved_client_runs() {
        let h = home();
        let d = retail_journal(&h, JournalVerdict::StepUnrecognized { step: 1 });
        let view = view_with(&h, Some(both(&h)), Some(vec![d]));
        let f = finding(ids::JOURNAL_STEP_UNRECOGNIZED, Some(h.rk));
        let retail = h.retail.clone();
        let refused = plan_with(&view, &f, &|_| running_at(&retail)).unwrap_err();
        assert_eq!(
            refused.reason,
            RefusalReason::ClientRunning,
            "involved_client_running"
        );
        assert!(
            refused.message.contains(&tx().to_string()),
            "{}",
            refused.message
        );
        let unknown = |_: &[Target]| RunningScan {
            clients: BTreeMap::new(),
            unavailable: Some("/proc could not be read".to_owned()),
        };
        assert_eq!(
            plan_with(&view, &f, &unknown).unwrap_err().reason,
            RefusalReason::ClientRunning,
            "process_table_unreadable"
        );
        let era = h.era.clone();
        assert_eq!(
            plan_with(&view, &f, &|_| running_at(&era)).unwrap().len(),
            1,
            "uninvolved_client_running"
        );

        // A state-only set with no client to guard is planned without a scan.
        let h = home();
        write(&h.paths.journal(&tx()), "{\"type\":\"prepared\"}\n");
        write(&h.paths.inflight_lock(&tx()), "");
        let d = diagnosis(
            h.paths.journal(&tx()),
            JournalFamily::Folder,
            None,
            JournalVerdict::Unattributed {
                cause: UnattributedCause::Unreadable,
            },
        );
        let view = view_with(&h, Some(Vec::new()), Some(vec![d]));
        let never = |_: &[Target]| -> RunningScan { panic!("a state-only set scans nothing") }; // ubs:ignore — fails the test
        let fixes = plan_with(&view, &finding(ids::JOURNAL_UNATTRIBUTED, None), &never).unwrap();
        assert!(
            suspended(&fixes)
                .artifacts
                .iter()
                .all(|a| a.root == QuarantineRoot::State)
        );
    }

    #[test]
    fn plan_is_empty_for_a_live_writer() {
        let h = home();
        let d = JournalDiagnosis {
            live: true,
            ..retail_journal(&h, JournalVerdict::StepUnrecognized { step: 1 })
        };
        let view = view_with(&h, Some(both(&h)), Some(vec![d]));
        let f = finding(ids::JOURNAL_STEP_UNRECOGNIZED, Some(h.rk));
        assert_eq!(plan_with(&view, &f, &idle).unwrap(), []);
    }

    /// Rows 3-8's remediation is the forced command, `-t` but for the machine's row.
    #[test]
    fn forced_remediation_by_row() {
        let rows = [
            (ids::JOURNAL_TOO_NEW, JournalVerdict::TooNew, true),
            (ids::JOURNAL_UNREADABLE, JournalVerdict::Unreadable, true),
            (
                ids::JOURNAL_UNEXPECTED_STATE,
                JournalVerdict::UnexpectedState { step: 1 },
                true,
            ),
            (
                ids::JOURNAL_STEP_UNRECOGNIZED,
                JournalVerdict::StepUnrecognized { step: 1 },
                true,
            ),
            (
                ids::JOURNAL_MARKER_MISMATCH,
                JournalVerdict::MarkerMismatch,
                true,
            ),
            (
                ids::JOURNAL_UNATTRIBUTED,
                JournalVerdict::Unattributed {
                    cause: UnattributedCause::Unreadable,
                },
                false,
            ),
        ];
        for (id, verdict, on_client) in rows {
            let h = home();
            let d = retail_journal(&h, verdict);
            let view = view_with(&h, Some(both(&h)), Some(vec![d]));
            let f = finding(id, on_client.then_some(h.rk));
            let spec = registry::row(id).unwrap().fix.unwrap();
            let target = on_client.then_some("retail");
            let got = crate::doctor::fix::remediation_for(&view, &f, &spec, target);
            let mut argv = vec!["omaddons", "doctor", "fix", "--only", id];
            let mut explain = vec!["omaddons", "doctor", "explain", id];
            if on_client {
                argv.extend(["-t", "retail"]);
                explain.extend(["-t", "retail"]);
            }
            argv.extend(["--force", "--yes"]);
            let owned = |v: &[&str]| v.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
            assert_eq!(got.argv, Some(owned(&argv)), "{id}");
            assert_eq!(got.command, shell_words::join(&argv), "{id}");
            assert_eq!(got.explain_argv, owned(&explain), "{id}");
            assert_eq!(
                (got.auto_fixable, got.needs_force, got.destructive),
                (false, true, false),
                "{id}"
            );
            assert_eq!(got.estimated_actions, 5, "{id}: one rename per artifact");
        }
    }

    /// E5 lowers the planned set-aside into one `rename_noreplace` per artifact, in order,
    /// each into its own side's quarantine, the guards on the journal file's alone; the
    /// steps validate, and lowering twice gives the same steps.
    #[test]
    fn suspend_lowers_in_order_with_guards_on_the_journal() {
        use crate::engine::repair::{LowerCtx, PlannedFix, RepairRun, lower};
        use crate::fsx::ScopeTable;
        let h = home();
        let d = retail_journal(&h, JournalVerdict::StepUnrecognized { step: 1 });
        let view = view_with(&h, Some(both(&h)), Some(vec![d]));
        let fixes = plan_with(
            &view,
            &finding(ids::JOURNAL_STEP_UNRECOGNIZED, Some(h.rk)),
            &idle,
        )
        .unwrap();
        let journal = suspended(&fixes).clone();
        let run = RepairRun {
            dir: h.paths.doctor_runs_dir().join("R"),
            id: "R".to_owned(),
        };
        mkdir(&run.dir);
        let scopes = ScopeTable::new(&h.paths, std::slice::from_ref(&h.retail), None).unwrap();
        let clients = [(h.retail.clone(), h.rk)];
        let repair_tx = crate::model::TxId::new();
        let ctx = LowerCtx {
            paths: &h.paths,
            tx: repair_tx,
            run: &run,
            clients: &clients,
            skill_dirs: &[],
            scopes: &scopes,
            mount_id: &crate::fsx::mount_id,
        };
        let input = [PlannedFix {
            fm: std::borrow::Cow::Borrowed(ids::JOURNAL_STEP_UNRECOGNIZED),
            fix: Fix::Suspend {
                journal: journal.clone(),
            },
        }];
        let lowered = lower(&ctx, &input).unwrap();
        let quarantine_state = run.dir.join("quarantine/state");
        let quarantine_client = ClientPaths::new(&h.retail)
            .snapshots_repair(&repair_tx)
            .join("quarantine");
        assert_eq!(lowered.steps.len(), journal.artifacts.len(), "{lowered:#?}");
        for (step, artifact) in lowered.steps.iter().zip(&journal.artifacts) {
            let Primitive::RenameNoReplace { from, to, .. } = &step.step.op else {
                panic!("{:#?}", step.step); // ubs:ignore — fails the test
            };
            assert_eq!(from, &artifact.path);
            let root = match artifact.root {
                QuarantineRoot::State => &quarantine_state,
                _ => &quarantine_client,
            };
            assert!(
                to.starts_with(root),
                "{} into {}",
                to.display(),
                root.display()
            );
            let is_journal = artifact.path == h.paths.journal(&tx());
            assert_eq!(!step.step.guards.is_empty(), is_journal, "{:#?}", step.step);
        }
        let steps: Vec<_> = lowered.steps.iter().map(|s| s.step.clone()).collect();
        crate::engine::repair_action::validate_steps(&lowered.fixes, &steps).unwrap();
        let again = lower(&ctx, &input).unwrap();
        let again: Vec<_> = again.steps.iter().map(|s| s.step.clone()).collect();
        assert_eq!(steps, again, "deterministic");
    }
}
