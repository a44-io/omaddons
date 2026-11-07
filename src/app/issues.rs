//! `status`'s issues (M3-D37): the doctor's findings, each shaped as `status` publishes
//! it, with vision D20's failure-mode id, a P0–P3 severity, a title and a remediation, so
//! the bar widget and agents read one ranked list instead of interpreting attention codes
//! (vision D2: the view holds no product logic). Pure: computed from findings.
//!
//! The findings are the registry's (F8, M4-D29): `status --cached` runs the doctor's
//! `cached` rows, plain `status` its `cached` and `quick` rows, through the doctor's own
//! detector loop, so `status` and `doctor` never disagree.

use std::collections::{BTreeMap, BTreeSet};

use schemars::JsonSchema;
use serde::Serialize;

use crate::doctor::detectors::concurrency;
use crate::doctor::finding::Finding;
use crate::model::TargetKey;

pub use crate::doctor::finding::Severity;
/// The failure-mode ids (vision D20): the registry's.
pub use crate::doctor::registry::ids;

/// The rows of an interrupted change (F1's 1-9): a client's attention item, and grouped
/// into one issue per row and scope.
pub const JOURNAL_ROWS: [&str; 9] = [
    ids::JOURNAL_PENDING,
    ids::JOURNAL_WAITS_FOR_GAME,
    ids::JOURNAL_TOO_NEW,
    ids::JOURNAL_UNREADABLE,
    ids::JOURNAL_UNEXPECTED_STATE,
    ids::JOURNAL_STEP_UNRECOGNIZED,
    ids::JOURNAL_MARKER_MISMATCH,
    ids::JOURNAL_UNATTRIBUTED,
    ids::JOURNAL_PRESERVATION_INCOMPLETE,
];

/// What addresses an issue: a command (`argv`), or prose (`argv` `null`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Remediation {
    /// The command for display, or the prose when there is no command.
    pub command: Option<String>,
    pub argv: Option<Vec<String>>,
    pub destructive: bool,
}

/// One finding about a target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Issue {
    /// A vision D20 failure-mode id ([`ids`]).
    pub id: &'static str,
    pub severity: Severity,
    pub title: String,
    /// The addon it is about, when it is about one.
    pub addon: Option<String>,
    pub remediation: Remediation,
}

/// The issues of no client: the machine's own, the bar widget's (E4) and a pending
/// journal that names no client. `status` lists them in `data.issues`, and
/// `summary.issues` counts each once.
pub fn for_machine(findings: &[Finding]) -> Vec<Issue> {
    ranked(findings.iter().filter(|f| f.target.is_none()))
}

/// The issues of the target whose key is `key`, ranked by severity, then id, then addon.
pub fn for_target(findings: &[Finding], key: TargetKey) -> Vec<Issue> {
    ranked(
        findings
            .iter()
            .filter(|f| f.target.as_ref().is_some_and(|t| t.key == key)),
    )
}

/// `status`'s issues from the doctor's findings: a finding about a client `status` lists
/// (`listed`, by key) is that client's; one about no client is the machine's
/// (`data.issues`); one about a client `status` does not list is the machine's too
/// without `-t`, so a P0 is never invisible, and out of scope with it (`scoped`, F1).
/// Each list is ranked as [`for_target`] ranks it.
pub fn assign(
    findings: &[Finding],
    listed: &BTreeSet<TargetKey>,
    scoped: bool,
) -> (BTreeMap<TargetKey, Vec<Issue>>, Vec<Issue>) {
    let mut clients: BTreeMap<TargetKey, Vec<&Finding>> = BTreeMap::new();
    let mut machine = Vec::new();
    for f in findings {
        match f.target.as_ref().map(|t| t.key) {
            Some(key) if listed.contains(&key) => clients.entry(key).or_default().push(f),
            Some(_) if scoped => {}
            _ => machine.push(f),
        }
    }
    let clients = clients
        .into_iter()
        .map(|(key, found)| (key, ranked(found.into_iter())))
        .collect();
    (clients, ranked(machine.into_iter()))
}

/// One issue per finding, except that a scope's journals, one finding each (M4-D30),
/// show as one issue per row: the first finding's own title for one, the row's plural
/// for more (F1). Ranked by severity, then id, then addon, then the (first) finding's
/// instance key, which never reaches the wire but keeps two issues of one row and scope
/// (two skill folders) in a stable order (M4-D30).
fn ranked<'a>(findings: impl Iterator<Item = &'a Finding>) -> Vec<Issue> {
    let mut journals: Vec<(&'static str, Vec<&Finding>)> = Vec::new();
    let mut issues: Vec<(Issue, &str)> = Vec::new();
    for f in findings {
        if !JOURNAL_ROWS.contains(&f.id) {
            issues.push((issue_of(f), f.instance.as_str()));
        } else if let Some((_, group)) = journals.iter_mut().find(|(id, _)| *id == f.id) {
            group.push(f);
        } else {
            journals.push((f.id, vec![f]));
        }
    }
    for (id, group) in journals {
        let Some(first) = group.first() else {
            continue;
        };
        let mut issue = issue_of(first);
        if group.len() > 1
            && let Some(plural) = concurrency::plural_title(id, group.len())
        {
            issue.title = plural;
        }
        issues.push((issue, first.instance.as_str()));
    }
    issues.sort_by(|(a, a_instance), (b, b_instance)| {
        (a.severity, a.id, &a.addon, a_instance).cmp(&(b.severity, b.id, &b.addon, b_instance))
    });
    issues.into_iter().map(|(issue, _)| issue).collect()
}

fn issue_of(f: &Finding) -> Issue {
    Issue {
        id: f.id,
        severity: f.severity,
        title: f.title.clone(),
        addon: f.addon.clone(),
        remediation: Remediation {
            command: Some(f.remediation.command.clone()),
            argv: f.remediation.argv.clone(),
            destructive: f.remediation.destructive,
        },
    }
}

/// How many of `issues` there are of each severity.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, JsonSchema)]
pub struct SeverityCounts {
    pub p0: usize,
    pub p1: usize,
    pub p2: usize,
    pub p3: usize,
}

impl SeverityCounts {
    pub fn add(&mut self, issues: &[Issue]) {
        for issue in issues {
            match issue.severity {
                Severity::P0 => self.p0 += 1,
                Severity::P1 => self.p1 += 1,
                Severity::P2 => self.p2 += 1,
                Severity::P3 => self.p3 += 1,
            }
        }
    }

    /// A P0 or P1 issue: the bar turns urgent.
    pub fn urgent(&self) -> bool {
        self.p0 + self.p1 > 0
    }

    pub fn total(&self) -> usize {
        self.p0 + self.p1 + self.p2 + self.p3
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};

    use ulid::Ulid;

    use super::*;
    use crate::app::drift::DriftItem;
    use crate::app::status::{Attention, StatusTarget, test_target};
    use crate::doctor::finding::{Confidence, Evidence, Observation, Subject};
    use crate::doctor::registry::{self, Tier};
    use crate::doctor::selection::{Pick, Selection};
    use crate::doctor::tests::fixture_paths;
    use crate::doctor::view::tests::{client, world_of};
    use crate::doctor::view::{DoctorView, Journals, LayerState, Preloaded, Records, WidgetLayer};
    use crate::drift::{AddonDrift, DriftMode, DriftReport, DriftState, FolderDrift};
    use crate::engine::journal::{Phase, Unrecoverable};
    use crate::engine::repair_action::JournalFamily;
    use crate::engine::simulate::{
        Diagnoses, Direction, JournalDiagnosis, JournalEvidence, JournalVerdict, ProtectedPaths,
        UnattributedCause,
    };
    use crate::fsx::LockState;
    use crate::model::{
        AddonReason, AddonRecord, AddonState, Channel, FolderName, TxId, VersionConfidence,
    };
    use crate::omarchy::status::WidgetInfo;
    use crate::state::records::TargetRecord;
    use crate::state::registry::{Origin, Registry, RegistryEntry};
    use crate::state::store::{Loaded, SchemaVersion, VersionStatus, Versioned};

    /// Where the cases' client lives.
    const CLIENT: &str = "/fixture/client";

    /// The registry's entry for the cases' client, `key` registered as `id`.
    fn entry(key: TargetKey, id: &str) -> RegistryEntry {
        RegistryEntry {
            key,
            id: id.to_owned(),
            install_root: PathBuf::from("/fixture"),
            client_dir: PathBuf::from(CLIENT),
            flavor_dir: Some("_retail_".to_owned()),
            product: Some("wow".to_owned()),
            qualifier: "test".to_owned(),
            origin: Origin::Discovered,
            registered_at: "2026-10-14T12:00:00Z".parse().unwrap(),
        }
    }

    /// A managed addon of the cases' record, in `state`, owning one folder of its name.
    fn addon(name: &str, state: AddonState) -> AddonRecord {
        AddonRecord {
            r#ref: "gh:example/Foo".parse().unwrap(),
            name: name.into(),
            version: "1.0".into(),
            version_confidence: VersionConfidence::Release,
            release_id: None,
            channel: Channel::Stable,
            pinned: false,
            reason: AddonReason::Explicit,
            required_by: vec![],
            folders: vec![FolderName::new(name).unwrap()],
            state,
            installed_at: "2026-10-14T21:00:00Z".parse().unwrap(),
            tx_id: "01JB8Q2VA00000000000000000".parse().unwrap(),
            blob_sha256: None,
            revision: None,
            left_out: Vec::new(),
            any_flavor: false,
        }
    }

    /// One drift item as `status` checked it: its one folder, gone when it is missing.
    fn addon_drift(item: &DriftItem) -> AddonDrift {
        AddonDrift {
            addon: item.addon.clone(),
            state: item.state,
            folders: vec![FolderDrift {
                folder: FolderName::new(&item.addon).unwrap(),
                present: item.state != DriftState::Missing,
                indexed: item.state != DriftState::Unindexed,
                changed: item.files,
                modified: Vec::new(),
                deleted: Vec::new(),
                added: Vec::new(),
                truncated: false,
            }],
        }
    }

    /// The view `status` would seed for `client` (a case's target, by `key`) and the
    /// machine. The client is discovered, or registered and gone when it is `missing`
    /// (then `status` checks no drift for it). Its record holds one degraded addon per
    /// `addons.degraded`, written by a newer omaddons when its attention says so; its drift
    /// is as `status` checked it (presence only under `--cached`); then the journals, a
    /// free lock and the widget.
    fn seeded_view(
        client_case: Option<(&StatusTarget, TargetKey)>,
        widget: Option<WidgetInfo>,
        journals: Journals,
        cached: bool,
    ) -> DoctorView {
        let paths = fixture_paths();
        let mut world = world_of(&paths, Vec::new());
        let mut registry = Registry::default();
        let mut records = BTreeMap::new();
        let mut drift = BTreeMap::new();
        if let Some((t, key)) = client_case {
            let entry = entry(key, &t.id);
            registry.targets.push(entry.clone());
            if t.missing {
                world.resolved.missing.push(entry);
            } else {
                let key_text = key.to_string();
                world
                    .resolved
                    .targets
                    .push(client(&t.id, Path::new(CLIENT), Some(&key_text)));
                let report = DriftReport {
                    mode: if cached {
                        DriftMode::Presence
                    } else {
                        DriftMode::Stat
                    },
                    files_checked: 0,
                    files_rehashed: 0,
                    addons: t
                        .drift
                        .iter()
                        .filter(|d| !cached || d.state == DriftState::Missing)
                        .map(addon_drift)
                        .collect(),
                };
                drift.insert(key, report);
            }
            let newer = t.attention.contains(&Attention::StateVersionNewer);
            let record = TargetRecord {
                generation: 1,
                addons: (0..t.addons.degraded)
                    .map(|n| {
                        let name = format!("Degraded{n}");
                        (
                            TargetRecord::addon_key(&name),
                            addon(&name, AddonState::Degraded),
                        )
                    })
                    .collect(),
                ..TargetRecord::empty(key)
            };
            let (version, status) = if newer {
                (SchemaVersion::new(2, 0), VersionStatus::Newer)
            } else {
                (TargetRecord::CURRENT, VersionStatus::Current)
            };
            records.insert(
                key,
                LayerState::Available(Loaded {
                    value: record,
                    version,
                    status,
                }),
            );
        }
        DoctorView::preloaded(
            paths,
            Preloaded {
                registry: Some(LayerState::Available(Loaded {
                    value: registry,
                    version: Registry::CURRENT,
                    status: VersionStatus::Current,
                })),
                records: Some(LayerState::Available(Records::from_states(records))),
                world: Some(LayerState::Available(world)),
                journals: Some(LayerState::Available(journals)),
                lock: Some(LayerState::Available(LockState::Free)),
                widget: Some(LayerState::Available(WidgetLayer { info: widget })),
                drift,
                ..Preloaded::default()
            },
        )
    }

    /// The doctor's loop over `status`'s tiers on `view`.
    fn found(view: &DoctorView, selection: &Selection, cached: bool) -> Vec<Finding> {
        let tiers: &[Tier] = if cached {
            &[Tier::Cached]
        } else {
            &[Tier::Cached, Tier::Quick]
        };
        crate::doctor::detect(view, selection, tiers)
            .unwrap()
            .findings
    }

    /// The registry path `status` takes, for one target: the view it would seed for the
    /// target as it reports it, the doctor's loop over its tiers, the target's issues. A
    /// target without a key gets the cases' key.
    pub(crate) fn registry_issues(
        target: &StatusTarget,
        journals: &[Journal],
        cached: bool,
    ) -> Vec<Issue> {
        let key = target.key.unwrap_or_else(|| KEY.parse().unwrap());
        let view = seeded_view(
            Some((target, key)),
            None,
            journals_of(journals, Some(key)),
            cached,
        );
        let found = found(&view, &Selection::all(), cached);
        let (mut clients, _) = assign(&found, &BTreeSet::from([key]), false);
        clients.remove(&key).unwrap_or_default()
    }

    /// The registry path `status` takes for the issues of no client. `widget-invalid` is
    /// left out: it validates the plugin's files, which a widget described by a
    /// [`WidgetInfo`] alone does not have (its own tests plant them).
    pub(crate) fn registry_machine_issues(
        widget: Option<&WidgetInfo>,
        journals: &[Journal],
    ) -> Vec<Issue> {
        let view = seeded_view(None, widget.cloned(), journals_of(journals, None), false);
        let selection = Selection {
            skip: vec![Pick::Mode(ids::WIDGET_INVALID)],
            ..Selection::all()
        };
        assign(&found(&view, &selection, false), &BTreeSet::new(), false).1
    }

    /// `journals` as E11 would diagnose them on `target` (`None`: naming no client). A
    /// settleable journal is a prepared folder journal rolled back (a doctor repair, on
    /// the machine); one that is too new or does not read is that, or unattributed when
    /// it names no client.
    fn journals_of(journals: &[Journal], target: Option<TargetKey>) -> Journals {
        Journals {
            pending: Vec::new(),
            diagnoses: Diagnoses {
                journals: journals
                    .iter()
                    .map(|&(tx, why)| diagnosis_of(tx, why, target))
                    .collect(),
                unlisted: None,
            },
            counts: BTreeMap::new(),
        }
    }

    fn diagnosis_of(
        tx: TxId,
        why: Option<Unrecoverable>,
        target: Option<TargetKey>,
    ) -> JournalDiagnosis {
        let verdict = match (why, target) {
            (None, _) => JournalVerdict::Recoverable {
                phase: Phase::Prepared,
                direction: Direction::Back,
            },
            (Some(Unrecoverable::TooNew), Some(_)) => JournalVerdict::TooNew,
            (Some(Unrecoverable::Unreadable), Some(_)) => JournalVerdict::Unreadable,
            (Some(Unrecoverable::TooNew), None) => JournalVerdict::Unattributed {
                cause: UnattributedCause::TooNew,
            },
            (Some(Unrecoverable::Unreadable), None) => JournalVerdict::Unattributed {
                cause: UnattributedCause::Unreadable,
            },
        };
        JournalDiagnosis {
            tx,
            family: if target.is_none() && why.is_none() {
                JournalFamily::Repair
            } else {
                JournalFamily::Folder
            },
            path: PathBuf::from(format!("/fixture/state/omaddons/journal/{tx}.jsonl")),
            target,
            client_dir: target.map(|_| PathBuf::from(CLIENT)),
            phase: Phase::Prepared,
            repair_phase: None,
            live: false,
            verdict,
            evidence: JournalEvidence {
                version: why.map(|w| match w {
                    Unrecoverable::TooNew => SchemaVersion::new(9, 0),
                    Unrecoverable::Unreadable => SchemaVersion::new(1, 1),
                }),
                ..JournalEvidence::default()
            },
            protected: ProtectedPaths::default(),
        }
    }

    fn drift(addon: &str, state: DriftState, files: u64) -> DriftItem {
        DriftItem {
            addon: addon.to_owned(),
            state,
            files,
            folders: None,
        }
    }

    fn argv(parts: &[&str]) -> Option<Vec<String>> {
        Some(parts.iter().map(|p| (*p).to_owned()).collect())
    }

    /// Every issue a discovered client can carry maps to its failure mode, ranked; a
    /// client whose directory is gone carries `target-vanished` and nothing its record or
    /// files would say.
    #[test]
    fn every_attention_maps_to_its_failure_mode() {
        let attention = [
            Attention::JournalPending,
            Attention::StateVersionNewer,
            Attention::AddonDegraded,
            Attention::AddonModified,
            Attention::AddonFolderMissing,
        ];
        let mut t = test_target("retail", 0, &attention, 0);
        t.addons.degraded = 2;
        t.drift = vec![
            drift("Plater", DriftState::Modified, 3),
            drift("Details", DriftState::Missing, 0),
            drift("Quiet", DriftState::Unindexed, 0),
        ];
        let issues = registry_issues(&t, &[recoverable(0)], false);
        let rows: Vec<(&str, Severity, &str, Option<&str>)> = issues
            .iter()
            .map(|i| (i.id, i.severity, i.title.as_str(), i.addon.as_deref()))
            .collect();
        assert_eq!(
            rows,
            [
                (
                    ids::JOURNAL_PENDING,
                    Severity::P0,
                    "An interrupted change waits to be finished",
                    None
                ),
                (
                    ids::STATE_VERSION_NEWER,
                    Severity::P0,
                    "State was written by a newer omaddons",
                    None
                ),
                (
                    ids::RECORD_FOLDER_MISSING,
                    Severity::P1,
                    "Details's folder is gone",
                    Some("Details")
                ),
                (
                    ids::MANAGED_FILES_MODIFIED,
                    Severity::P2,
                    "Plater's files changed on disk (3 files)",
                    Some("Plater")
                ),
                (
                    ids::OWNERSHIP_TRANSFERRED,
                    Severity::P2,
                    "2 addons lost a folder to another addon",
                    None
                ),
            ],
            "ranked by severity, then id; an unindexed addon is no issue"
        );
        let remediation = |id: &str| &issues.iter().find(|i| i.id == id).unwrap().remediation;
        let reinstall =
            |addon| argv(&["omaddons", "install", addon, "-t", "retail", "--reinstall"]);
        assert_eq!(
            remediation(ids::RECORD_FOLDER_MISSING).argv,
            reinstall("Details")
        );
        assert_eq!(
            remediation(ids::MANAGED_FILES_MODIFIED).argv,
            reinstall("Plater")
        );
        assert_eq!(
            remediation(ids::OWNERSHIP_TRANSFERRED).argv,
            argv(&["omaddons", "list", "-t", "retail", "-v"])
        );
        // A journal recovery settles is the delegation's (F2, M4-D53).
        assert_eq!(
            remediation(ids::JOURNAL_PENDING).argv,
            argv(&[
                "omaddons",
                "doctor",
                "fix",
                "--only",
                ids::JOURNAL_PENDING,
                "-t",
                "retail"
            ])
        );
        assert_eq!(remediation(ids::STATE_VERSION_NEWER).argv, None);
        assert!(remediation(ids::STATE_VERSION_NEWER).command.is_some());

        let mut gone = test_target(
            "retail",
            0,
            &[Attention::TargetMissing, Attention::AddonDegraded],
            0,
        );
        gone.addons.degraded = 2;
        gone.drift = vec![drift("Details", DriftState::Missing, 0)];
        let issues = registry_issues(&gone, &[], false);
        let ids: Vec<&str> = issues.iter().map(|i| i.id).collect();
        assert_eq!(ids, [ids::TARGET_VANISHED], "{issues:?}");
        let forget = &issues[0].remediation;
        assert_eq!(forget.argv, argv(&["omaddons", "targets", "rm", "retail"]));
        assert!(forget.destructive);
        assert_eq!(
            forget.command.as_deref(),
            Some("omaddons targets rm retail")
        );
    }

    /// A stuck journal names its state, and its remediation is its forced set-aside (F2
    /// s3, as `doctor` shows it), which parses; machine-scoped, it has no `-t`.
    #[test]
    fn stuck_rows_remediate_with_their_set_aside() {
        use clap::Parser;
        let mut t = test_target("retail", 0, &[Attention::JournalPending], 0);
        t.key = Some(KEY.parse().unwrap());
        for (why, id, title) in [
            (
                Unrecoverable::TooNew,
                ids::JOURNAL_TOO_NEW,
                "An interrupted change was started by a newer omaddons; this build cannot read its journal (format 9.0)",
            ),
            (
                Unrecoverable::Unreadable,
                ids::JOURNAL_UNREADABLE,
                "An interrupted change's journal holds lines this omaddons cannot read",
            ),
        ] {
            let issues = registry_issues(&t, &[unrecoverable(0, why)], false);
            let issue = issues.iter().find(|i| i.id == id).unwrap();
            assert_eq!(issue.title, title, "{why:?}");
            let argv = issue.remediation.argv.clone().unwrap();
            assert_eq!(
                argv,
                [
                    "omaddons", "doctor", "fix", "--only", id, "-t", "retail", "--force", "--yes"
                ]
            );
            assert!(crate::cli::Cli::try_parse_from(&argv).is_ok(), "{argv:?}");
            assert!(!issue.remediation.destructive);
        }
        let machine = registry_machine_issues(None, &[unrecoverable(0, Unrecoverable::Unreadable)]);
        assert_eq!(
            machine[0].remediation.argv.as_deref(),
            Some(
                &[
                    "omaddons",
                    "doctor",
                    "fix",
                    "--only",
                    ids::JOURNAL_UNATTRIBUTED,
                    "--force",
                    "--yes"
                ]
                .map(str::to_owned)[..]
            ),
            "machine-scoped: no -t"
        );
    }

    /// A journal recovery can settle is remediated by the doctor's delegation, as
    /// `doctor` shows it (M4-D53: `status` no longer says "any change command"); the
    /// argv parses.
    #[test]
    fn a_recoverable_row_is_the_doctors_fix() {
        use clap::Parser;
        let mut t = test_target("retail", 0, &[Attention::JournalPending], 0);
        t.key = Some(KEY.parse().unwrap());
        let other = registry_issues(&t, &[recoverable(0)], false);
        assert_eq!(other[0].title, "An interrupted change waits to be finished");
        let argv = other[0].remediation.argv.clone().unwrap();
        assert_eq!(
            argv,
            [
                "omaddons",
                "doctor",
                "fix",
                "--only",
                ids::JOURNAL_PENDING,
                "-t",
                "retail"
            ]
        );
        assert!(crate::cli::Cli::try_parse_from(&argv).is_ok(), "{argv:?}");
        assert!(!other[0].remediation.destructive);
    }

    #[test]
    fn one_file_and_one_addon_read_singular() {
        let mut t = test_target(
            "retail",
            0,
            &[Attention::AddonDegraded, Attention::AddonModified],
            0,
        );
        t.drift = vec![drift("Plater", DriftState::Modified, 1)];
        let titles: Vec<String> = registry_issues(&t, &[], false)
            .into_iter()
            .map(|i| i.title)
            .collect();
        assert!(titles.contains(&"Plater's files changed on disk (1 file)".to_owned()));
        assert!(titles.contains(&"1 addon lost a folder to another addon".to_owned()));
    }

    #[test]
    fn severities_serialize_as_p0_to_p3_and_count() {
        assert_eq!(serde_json::to_value(Severity::P0).unwrap(), "P0");
        assert_eq!(serde_json::to_value(Severity::P3).unwrap(), "P3");
        let t = test_target(
            "retail",
            0,
            &[Attention::JournalPending, Attention::TargetMissing],
            0,
        );
        let mut counts = SeverityCounts::default();
        counts.add(&registry_issues(&t, &[recoverable(0)], false));
        assert_eq!((counts.p0, counts.p2, counts.total()), (1, 1, 2));
        assert!(counts.urgent());
    }

    /// A finding of `id` about `subject`, as its row makes it.
    fn finding_of(id: &str, subject: Subject) -> Finding {
        registry::row(id).unwrap().finding(Observation {
            subject,
            title: "t".to_owned(),
            confidence: Confidence::High,
            addon: None,
            folder: None,
            evidence: Evidence::default(),
        })
    }

    /// Findings go to a listed client, the machine, or nowhere under `-t` (F1's rule).
    #[test]
    fn findings_are_assigned_by_case() {
        let listed: TargetKey = KEY.parse().unwrap();
        let other: TargetKey = "01M7A1B2C3D4E5F6G7H8J9K0MN".parse().unwrap();
        let on = |key: TargetKey| finding_of(ids::TARGET_VANISHED, Subject::target("c", key));
        let machine = finding_of(ids::LOCK_HELD_LONG, Subject::machine());
        let keys = BTreeSet::from([listed]);
        for (case, finding, scoped, client, on_machine) in [
            ("listed_client", on(listed), false, 1, 0),
            ("listed_client_with_t", on(listed), true, 1, 0),
            ("machine", machine.clone(), false, 0, 1),
            ("machine_with_t", machine, true, 0, 1),
            ("unlisted_key_without_t", on(other), false, 0, 1),
            ("unlisted_key_with_t", on(other), true, 0, 0),
        ] {
            let (clients, issues) = assign(std::slice::from_ref(&finding), &keys, scoped);
            let got = (clients.get(&listed).map_or(0, Vec::len), issues.len());
            assert_eq!(
                got,
                (client, on_machine),
                "{case}: {finding:?} gave {clients:?} and {issues:?}"
            );
            assert!(
                clients.keys().all(|key| *key == listed),
                "{case}: only a listed client holds issues: {clients:?}"
            );
        }
    }

    /// Two issues equal but for their findings' instances keep the instances' order,
    /// whatever order the findings came in.
    #[test]
    fn issues_sort_by_severity_id_addon_then_instance() {
        let at = |path: &str| {
            let mut f = finding_of(
                ids::SKILL_VERSION_DRIFT,
                Subject::machine().path(Path::new(path)),
            );
            f.title = path.to_owned();
            f
        };
        let lock = finding_of(ids::LOCK_HELD_LONG, Subject::machine());
        for (case, findings) in [
            (
                "in_order",
                vec![at("/a/SKILL.md"), at("/b/SKILL.md"), lock.clone()],
            ),
            ("reversed", vec![lock, at("/b/SKILL.md"), at("/a/SKILL.md")]),
        ] {
            let titles: Vec<String> = for_machine(&findings)
                .into_iter()
                .map(|i| i.title)
                .collect();
            assert_eq!(
                titles,
                ["t", "/a/SKILL.md", "/b/SKILL.md"],
                "{case}: the P2 lock first, then the P3 skills by instance"
            );
        }
    }

    // ---- Characterization (E1 part 1): what `status` emits for every issue it can
    // produce, pinned in snapshots. Every case goes through `issues_of_case` or
    // `machine_issues_of_case`, which since F8 run the doctor's loop on the view `status`
    // would seed for the case.

    /// A pending journal naming a target: its transaction, and why recovery cannot
    /// settle it by itself, when it cannot.
    pub(crate) type Journal = (TxId, Option<Unrecoverable>);

    /// One target as `status` reports it, with what the registry path will also read.
    pub(crate) struct Case {
        pub(crate) name: &'static str,
        pub(crate) target: StatusTarget,
        /// The pending journals naming the target, oldest first, one finding each.
        pub(crate) journals: Vec<Journal>,
        /// `status --cached`, which checks drift by presence only.
        pub(crate) cached: bool,
    }

    /// The issues of no client: the bar widget and the journals that name no client.
    pub(crate) struct MachineCase {
        pub(crate) name: &'static str,
        pub(crate) widget: Option<WidgetInfo>,
        pub(crate) journals: Vec<Journal>,
    }

    /// The key every case's target carries.
    const KEY: &str = "01M5ZQ7Y3KJ8V9W2X4C6B0N1RT";

    /// The first, second and third pending journal of a case.
    fn tx(n: usize) -> TxId {
        let id = [
            "01M7A1B2C3D4E5F6G7H8J9K0MN",
            "01M7A1B2C3D4E5F6G7H8J9K0MP",
            "01M7A1B2C3D4E5F6G7H8J9K0MQ",
        ][n];
        TxId::from_ulid(Ulid::from_string(id).unwrap())
    }

    fn recoverable(n: usize) -> Journal {
        (tx(n), None)
    }

    fn unrecoverable(n: usize, why: Unrecoverable) -> Journal {
        (tx(n), Some(why))
    }

    /// A retail case: `attention`, then `edit` on its target.
    fn case(
        name: &'static str,
        attention: &[Attention],
        journals: Vec<Journal>,
        edit: impl FnOnce(&mut StatusTarget),
    ) -> Case {
        let mut target = test_target("retail", 0, attention, 0);
        target.key = Some(KEY.parse::<TargetKey>().unwrap());
        edit(&mut target);
        Case {
            name,
            target,
            journals,
            cached: false,
        }
    }

    /// Every issue `status` can raise for a target, each case by name.
    pub(crate) fn cases() -> Vec<Case> {
        use Attention::*;
        // Every attention a discovered client can raise at once: a client whose
        // directory is gone has no files to check (`vanished_*`).
        let all = [
            JournalPending,
            StateVersionNewer,
            AddonDegraded,
            AddonModified,
            AddonFolderMissing,
        ];
        let mut cached = case(
            "files_modified_under_cached",
            &[AddonModified],
            vec![],
            |t| {
                t.drift = vec![drift("Plater", DriftState::Modified, 3)];
            },
        );
        cached.cached = true;
        vec![
            case(
                "journal_waits",
                &[JournalPending],
                vec![recoverable(0)],
                |_| {},
            ),
            case(
                "journal_too_new",
                &[JournalPending],
                vec![unrecoverable(0, Unrecoverable::TooNew)],
                |_| {},
            ),
            case(
                "journal_unreadable",
                &[JournalPending],
                vec![unrecoverable(0, Unrecoverable::Unreadable)],
                |_| {},
            ),
            case(
                "two_journals_one_unreadable",
                &[JournalPending],
                vec![recoverable(0), unrecoverable(1, Unrecoverable::Unreadable)],
                |_| {},
            ),
            case(
                "two_journals_both_recoverable",
                &[JournalPending],
                vec![recoverable(0), recoverable(1)],
                |_| {},
            ),
            case("state_newer", &[StateVersionNewer], vec![], |_| {}),
            case("folder_missing", &[AddonFolderMissing], vec![], |t| {
                t.drift = vec![drift("Details", DriftState::Missing, 0)];
            }),
            case("files_modified_one", &[AddonModified], vec![], |t| {
                t.drift = vec![drift("Plater", DriftState::Modified, 1)];
            }),
            case("files_modified_three", &[AddonModified], vec![], |t| {
                t.drift = vec![drift("Plater", DriftState::Modified, 3)];
            }),
            cached,
            case("degraded_one", &[AddonDegraded], vec![], |t| {
                t.addons.degraded = 1;
            }),
            case("degraded_two", &[AddonDegraded], vec![], |t| {
                t.addons.degraded = 2;
            }),
            case("vanished", &[TargetMissing], vec![], |t| t.missing = true),
            case(
                "vanished_with_journal",
                &[JournalPending, TargetMissing],
                vec![recoverable(0)],
                |t| t.missing = true,
            ),
            // A missing target's record may hold degraded addons, but `status` raises no
            // `addon_degraded` attention for it, and the doctor no ownership finding.
            case(
                "vanished_with_degraded_record",
                &[TargetMissing],
                vec![],
                |t| {
                    t.missing = true;
                    t.addons.degraded = 2;
                },
            ),
            case("unindexed_is_no_issue", &[], vec![], |t| {
                t.drift = vec![drift("Quiet", DriftState::Unindexed, 0)];
            }),
            case(
                "everything",
                &all,
                vec![recoverable(0), unrecoverable(1, Unrecoverable::TooNew)],
                |t| {
                    t.addons.degraded = 2;
                    t.drift = vec![
                        drift("Details", DriftState::Missing, 0),
                        drift("Plater", DriftState::Modified, 3),
                        drift("Quiet", DriftState::Unindexed, 0),
                        drift("Aaa", DriftState::Missing, 0),
                    ];
                },
            ),
        ]
    }

    /// The case named `name`.
    pub(crate) fn named(name: &str) -> Case {
        cases().into_iter().find(|c| c.name == name).unwrap()
    }

    /// An installed bar widget on an Omarchy desktop, from another build, installed
    /// under this run's directories (`context_matches`) or another's.
    fn outdated_widget(context_matches: bool) -> WidgetInfo {
        WidgetInfo {
            omarchy: true,
            installed: true,
            enabled: true,
            version: Some("0.0.9".to_owned()),
            outdated: true,
            context_matches: Some(context_matches),
        }
    }

    /// The journals that name no client: one that does not read, and one whose file could
    /// not be read at all.
    fn unattributed() -> Vec<Journal> {
        vec![unrecoverable(0, Unrecoverable::Unreadable), recoverable(1)]
    }

    /// Every issue of no client, each case by name.
    pub(crate) fn machine_cases() -> Vec<MachineCase> {
        let machine = |name, widget, journals| MachineCase {
            name,
            widget,
            journals,
        };
        vec![
            machine("machine_nothing", None, vec![]),
            machine(
                "machine_widget_outdated",
                Some(outdated_widget(true)),
                vec![],
            ),
            machine(
                "machine_widget_other_context",
                Some(outdated_widget(false)),
                vec![],
            ),
            machine("machine_unattributed", None, unattributed()),
            machine("machine_both", Some(outdated_widget(true)), unattributed()),
        ]
    }

    /// The machine case named `name`.
    pub(crate) fn machine_named(name: &str) -> MachineCase {
        machine_cases()
            .into_iter()
            .find(|c| c.name == name)
            .unwrap()
    }

    /// What `status` shows for a case: the registry's findings for the view it would
    /// seed, one per journal, mapped to issues.
    pub(crate) fn issues_of_case(case: &Case) -> Vec<Issue> {
        registry_issues(&case.target, &case.journals, case.cached)
    }

    /// What `status` shows of no client for a machine case.
    pub(crate) fn machine_issues_of_case(case: &MachineCase) -> Vec<Issue> {
        registry_machine_issues(case.widget.as_ref(), &case.journals)
    }

    /// One issue per row and client: the instance's title for one, the plural for more.
    #[test]
    fn journal_issues_group_per_client_and_row() {
        let mut t = test_target("retail", 0, &[Attention::JournalPending], 0);
        t.key = Some(KEY.parse().unwrap());
        let issues = registry_issues(
            &t,
            &[
                unrecoverable(0, Unrecoverable::TooNew),
                unrecoverable(1, Unrecoverable::TooNew),
                recoverable(2),
            ],
            false,
        );
        let journal: Vec<(&str, &str)> = issues
            .iter()
            .filter(|i| JOURNAL_ROWS.contains(&i.id))
            .map(|i| (i.id, i.title.as_str()))
            .collect();
        assert_eq!(
            journal,
            [
                (
                    ids::JOURNAL_PENDING,
                    "An interrupted change waits to be finished"
                ),
                (
                    ids::JOURNAL_TOO_NEW,
                    "2 interrupted changes were started by a newer omaddons"
                ),
            ],
            "{issues:?}"
        );
        let waits = registry_issues(&t, &[recoverable(0), recoverable(1)], false);
        let pending: Vec<&Issue> = waits
            .iter()
            .filter(|i| i.id == ids::JOURNAL_PENDING)
            .collect();
        assert_eq!(pending.len(), 1, "{waits:?}");
        assert_eq!(
            pending[0].title,
            "2 interrupted changes wait to be finished"
        );
    }

    /// A held lock hides every journal (a change is committing); a holder older than
    /// `LOCK_HELD_LONG` is the machine's `lock-held-long`, never a journal row.
    #[test]
    fn a_held_lock_hides_journal_findings() {
        use crate::fsx::HolderInfo;
        let key: TargetKey = KEY.parse().unwrap();
        let journals = || Journals {
            pending: Vec::new(),
            diagnoses: Diagnoses {
                journals: vec![
                    diagnosis_of(tx(0), None, Some(key)),
                    diagnosis_of(tx(1), Some(Unrecoverable::Unreadable), None),
                ],
                unlisted: None,
            },
            counts: BTreeMap::new(),
        };
        let mut rows: Vec<&'static str> = JOURNAL_ROWS.to_vec();
        rows.push(ids::LOCK_HELD_LONG);
        let selection = Selection::only(&rows);
        let run = |lock: LockState| {
            let view = DoctorView::preloaded(
                fixture_paths(),
                Preloaded {
                    journals: Some(LayerState::Available(journals())),
                    lock: Some(LayerState::Available(lock)),
                    ..Preloaded::default()
                },
            );
            found(&view, &selection, true)
        };
        let free = run(LockState::Free);
        assert_eq!(free.len(), 2, "{free:?}");
        let held = LockState::Held {
            holder: Some(HolderInfo {
                pid: 7,
                command: "omaddons update".to_owned(),
                started_at: "2026-10-31T11:00:00Z".parse().unwrap(),
            }),
        };
        let under_held = run(held);
        let ids: Vec<&str> = under_held.iter().map(|f| f.id).collect();
        assert_eq!(
            ids,
            [ids::LOCK_HELD_LONG],
            "lock_held_long_is_a_machine_issue"
        );
        let issues = for_machine(&under_held);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].remediation.argv, None, "advice only");
        assert!(!JOURNAL_ROWS.contains(&issues[0].id), "never attention");
    }

    #[test]
    fn issues_by_case_are_pinned() {
        let by_case: BTreeMap<&str, Vec<Issue>> = cases()
            .iter()
            .map(|c| (c.name, issues_of_case(c)))
            .collect();
        insta::assert_json_snapshot!("status_issues_by_case", by_case);
    }

    #[test]
    fn machine_issues_by_case_are_pinned() {
        let by_case: BTreeMap<&str, Vec<Issue>> = machine_cases()
            .iter()
            .map(|c| (c.name, machine_issues_of_case(c)))
            .collect();
        insta::assert_json_snapshot!("status_machine_issues_by_case", by_case);
    }
}
