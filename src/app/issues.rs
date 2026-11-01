//! `status`'s issues (M3-D37): each attention item as a finding shaped like the M4
//! doctor's, with vision D20's failure-mode id, a P0–P3 severity, a title and a
//! remediation, so the bar widget and agents read one ranked list instead of
//! interpreting attention codes (vision D2: the view holds no product logic). The
//! remediations come from the same hints as `status`'s `commands`, so the two never
//! disagree. Pure: computed from a target as `status` reports it.
//!
//! Each issue is a finding of the doctor's failure-mode registry (E1), mapped to the
//! shape `status` published before the registry existed.

use schemars::JsonSchema;
use serde::Serialize;

use crate::doctor::detectors::concurrency;
use crate::doctor::finding::Finding;
use crate::model::TargetKey;

pub use crate::doctor::finding::Severity;
/// The failure-mode ids (vision D20): the registry's.
pub use crate::doctor::registry::ids;

/// The failure modes `status` runs: the seven it publishes, and the journal and lock
/// rows `journal-pending` was split into (F1), so its output does not change while the
/// rest of the doctor's catalog grows, until F8 moves it onto every `cached` and `quick`
/// row.
pub const STATUS_FAILURE_MODES: &[&str] = &[
    ids::JOURNAL_PENDING,
    ids::STATE_VERSION_NEWER,
    ids::RECORD_FOLDER_MISSING,
    ids::MANAGED_FILES_MODIFIED,
    ids::OWNERSHIP_TRANSFERRED,
    ids::TARGET_VANISHED,
    ids::WIDGET_VERSION_DRIFT,
    ids::JOURNAL_WAITS_FOR_GAME,
    ids::JOURNAL_TOO_NEW,
    ids::JOURNAL_UNREADABLE,
    ids::JOURNAL_UNEXPECTED_STATE,
    ids::JOURNAL_STEP_UNRECOGNIZED,
    ids::JOURNAL_MARKER_MISMATCH,
    ids::JOURNAL_UNATTRIBUTED,
    ids::JOURNAL_PRESERVATION_INCOMPLETE,
    ids::LOCK_HELD_LONG,
];

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

/// One issue per finding, except that a scope's journals, one finding each (M4-D30),
/// show as one issue per row: the finding's own title for one, the row's plural for
/// more (F1). Ranked by severity, then id, then addon.
fn ranked<'a>(findings: impl Iterator<Item = &'a Finding>) -> Vec<Issue> {
    let mut journals: Vec<(&'static str, Vec<&Finding>)> = Vec::new();
    let mut issues = Vec::new();
    for f in findings {
        if !JOURNAL_ROWS.contains(&f.id) {
            issues.push(issue_of(f));
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
        issues.push(issue);
    }
    issues.sort_by(|a, b| (a.severity, a.id, &a.addon).cmp(&(b.severity, b.id, &b.addon)));
    issues
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

/// `status`'s issues as they were computed before the failure-mode registry (verbatim):
/// the oracle the registry path is held to.
#[cfg(test)]
pub(crate) mod legacy {
    use super::{Issue, Remediation, Severity, ids};
    use crate::app::drift::reinstall_hint;
    use crate::app::status::{Attention, StatusTarget, forget_target_hint, inspect_hint};
    use crate::drift::DriftState;
    use crate::error::CommandHint;
    use crate::omarchy::status::{self as omarchy_status, WidgetInfo};

    /// The `journal_pending` reasons a recovery cannot settle by itself: the change needs
    /// manual repair.
    const MANUAL_REPAIR: &[&str] = &[
        "unexpected_state",
        "journal_unreadable",
        "journal_too_new",
        "preservation_incomplete",
    ];

    impl Remediation {
        fn hint(hint: &CommandHint) -> Self {
            Self {
                command: Some(hint.command().to_owned()),
                argv: Some(hint.argv().to_vec()),
                destructive: hint.destructive(),
            }
        }

        fn prose(text: &str) -> Self {
            Self {
                command: Some(text.to_owned()),
                argv: None,
                destructive: false,
            }
        }
    }

    /// The issues of no client: the machine's own, the bar widget's (E4) and a pending
    /// journal that names no client. `status` lists them in `data.issues`, and
    /// `summary.issues` counts each once.
    pub(crate) fn machine_issues(
        widget: Option<&WidgetInfo>,
        unattributed_journal: bool,
    ) -> Vec<Issue> {
        let mut out = Vec::new();
        // A pending journal that names no client (it cannot be read): no target can carry
        // it, and nothing recovers it by itself.
        if unattributed_journal {
            out.push(Issue {
                id: ids::JOURNAL_PENDING,
                severity: Severity::P0,
                title: "An interrupted change needs manual repair".to_owned(),
                addon: None,
                remediation: Remediation::prose(
                    "see \"journal_pending that does not clear\" under Troubleshooting in the README",
                ),
            });
        }
        if widget.is_some_and(WidgetInfo::needs_reinstall) {
            out.push(Issue {
                id: ids::WIDGET_VERSION_DRIFT,
                severity: Severity::P3,
                title: "The bar widget is from another omaddons build".to_owned(),
                addon: None,
                remediation: Remediation::hint(&omarchy_status::reinstall_hint()),
            });
        }
        out
    }

    /// `target`'s issues, ranked by severity, then id, then addon. `journal_reason` is why
    /// a pending journal cannot be recovered by itself, when it cannot
    /// (`unexpected_state`, `journal_unreadable`, `journal_too_new`,
    /// `preservation_incomplete`).
    pub(crate) fn issues_for(target: &StatusTarget, journal_reason: Option<&str>) -> Vec<Issue> {
        let mut issues = Vec::new();
        for attention in &target.attention {
            match attention {
                Attention::JournalPending => {
                    let manual = journal_reason.is_some_and(|r| MANUAL_REPAIR.contains(&r));
                    let (title, remediation) = if manual {
                        (
                            "An interrupted change needs manual repair",
                            Remediation::prose(
                                "see \"journal_pending that does not clear\" under Troubleshooting in the README",
                            ),
                        )
                    } else {
                        // The doctor's delegation (F2, M4-D53), as `doctor` shows it.
                        let argv: Vec<String> = [
                            "omaddons",
                            "doctor",
                            "fix",
                            "--only",
                            ids::JOURNAL_PENDING,
                            "-t",
                            target.id.as_str(),
                        ]
                        .map(str::to_owned)
                        .to_vec();
                        (
                            "An interrupted change waits to be finished",
                            Remediation {
                                command: Some(shell_words::join(&argv)),
                                argv: Some(argv),
                                destructive: false,
                            },
                        )
                    };
                    issues.push(Issue {
                        id: ids::JOURNAL_PENDING,
                        severity: Severity::P0,
                        title: title.to_owned(),
                        addon: None,
                        remediation,
                    });
                }
                Attention::StateVersionNewer => issues.push(Issue {
                    id: ids::STATE_VERSION_NEWER,
                    severity: Severity::P0,
                    title: "State was written by a newer omaddons".to_owned(),
                    addon: None,
                    remediation: Remediation::prose("upgrade omaddons"),
                }),
                Attention::TargetMissing => issues.push(Issue {
                    id: ids::TARGET_VANISHED,
                    severity: Severity::P2,
                    title: "The client's directory is gone".to_owned(),
                    addon: None,
                    remediation: Remediation::hint(&forget_target_hint(&target.id)),
                }),
                Attention::AddonDegraded => {
                    let n = target.addons.degraded;
                    let addons = if n == 1 { "addon" } else { "addons" };
                    issues.push(Issue {
                        id: ids::OWNERSHIP_TRANSFERRED,
                        severity: Severity::P2,
                        title: format!("{n} {addons} lost a folder to another addon"),
                        addon: None,
                        remediation: Remediation::hint(&inspect_hint(&target.id)),
                    });
                }
                // One per addon, from the drift.
                Attention::AddonModified | Attention::AddonFolderMissing => {}
            }
        }
        for drift in &target.drift {
            let reinstall = Remediation::hint(&reinstall_hint(&drift.addon, &target.id));
            let issue = match drift.state {
                DriftState::Missing => Issue {
                    id: ids::RECORD_FOLDER_MISSING,
                    severity: Severity::P1,
                    title: format!("{}'s folder is gone", drift.addon),
                    addon: Some(drift.addon.clone()),
                    remediation: reinstall,
                },
                DriftState::Modified => {
                    let files = if drift.files == 1 { "file" } else { "files" };
                    Issue {
                        id: ids::MANAGED_FILES_MODIFIED,
                        severity: Severity::P2,
                        title: format!(
                            "{}'s files changed on disk ({} {files})",
                            drift.addon, drift.files
                        ),
                        addon: Some(drift.addon.clone()),
                        remediation: reinstall,
                    }
                }
                DriftState::Unindexed | DriftState::Clean => continue,
            };
            issues.push(issue);
        }
        issues.sort_by(|a, b| (a.severity, a.id, &a.addon).cmp(&(b.severity, b.id, &b.addon)));
        issues
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use proptest::prelude::*;
    use ulid::Ulid;

    use super::*;
    use crate::app::drift::DriftItem;
    use crate::app::status::{
        Attention, StatusTarget, journal_findings, machine_facts, target_facts, test_target,
    };
    use crate::doctor::facts::{MachineFacts, NewerFile, StatusFacts};
    use crate::doctor::registry::Tier;
    use crate::doctor::view::Journals;
    use crate::drift::DriftState;
    use crate::engine::journal::{Phase, Unrecoverable};
    use crate::engine::repair_action::JournalFamily;
    use crate::engine::simulate::{
        Diagnoses, Direction, JournalDiagnosis, JournalEvidence, JournalVerdict, ProtectedPaths,
        UnattributedCause,
    };
    use crate::fsx::LockState;
    use crate::model::TxId;
    use crate::omarchy::status::WidgetInfo;
    use crate::state::records::TargetRecord;
    use crate::state::store::{SchemaVersion, Versioned};

    /// The registry path `status` takes, for one target: its facts as `status` builds
    /// them from the target and its pending journals, every row up to the tier, the
    /// target's issues. A target without a key gets the cases' key.
    pub(crate) fn registry_issues(
        target: &StatusTarget,
        journals: &[Journal],
        cached: bool,
    ) -> Vec<Issue> {
        let mut target = target.clone();
        if cached {
            target.drift.retain(|d| d.state == DriftState::Missing);
        }
        let key = target.key.unwrap_or_else(|| KEY.parse().unwrap());
        let facts = target_facts(
            &target,
            key,
            Some(NewerFile {
                path: PathBuf::from(format!("/fixture/state/omaddons/targets/{key}.json")),
                on_disk: SchemaVersion::new(2, 0),
                writes: <TargetRecord as Versioned>::CURRENT,
            }),
            Some(PathBuf::from("/fixture/client")),
            &|addon| {
                vec![PathBuf::from(format!(
                    "/fixture/client/Interface/AddOns/{addon}"
                ))]
            },
        );
        let mut found = detect_with(vec![facts], MachineFacts::default(), cached);
        found.extend(journal_issues_found(journals, Some(key)));
        for_target(&found, key)
    }

    /// The registry path `status` takes for the issues of no client.
    pub(crate) fn registry_machine_issues(
        widget: Option<&WidgetInfo>,
        journals: &[Journal],
    ) -> Vec<Issue> {
        let mut found = detect_with(Vec::new(), machine_facts(widget), false);
        found.extend(journal_issues_found(journals, None));
        for_machine(&found)
    }

    /// `journals` as E11 would diagnose them on `target` (`None`: naming no client), and
    /// `status`'s findings of them under a free lock. A settleable journal is a prepared
    /// folder journal rolled back (a doctor repair, on the machine); one that is too new
    /// or does not read is that, or unattributed when it names no client.
    fn journal_issues_found(journals: &[Journal], target: Option<TargetKey>) -> Vec<Finding> {
        let diagnoses = journals
            .iter()
            .map(|&(tx, why)| diagnosis_of(tx, why, target))
            .collect();
        let journals = Journals {
            pending: Vec::new(),
            diagnoses: Diagnoses {
                journals: diagnoses,
                unlisted: None,
            },
            counts: BTreeMap::new(),
        };
        let target_id = |key: TargetKey| {
            if key.to_string() == KEY {
                "retail".to_owned()
            } else {
                key.to_string()
            }
        };
        journal_findings(
            &journals,
            &LockState::Free,
            &target_id,
            "2026-10-31T12:00:00Z".parse().unwrap(),
        )
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
            client_dir: target.map(|_| PathBuf::from("/fixture/client")),
            phase: Phase::Prepared,
            repair_phase: None,
            live: false,
            verdict,
            evidence: JournalEvidence {
                version: why.map(|w| match w {
                    Unrecoverable::TooNew => crate::state::store::SchemaVersion::new(9, 0),
                    Unrecoverable::Unreadable => crate::state::store::SchemaVersion::new(1, 1),
                }),
                ..JournalEvidence::default()
            },
            protected: ProtectedPaths::default(),
        }
    }

    fn detect_with(
        targets: Vec<crate::doctor::facts::TargetFacts>,
        machine: MachineFacts,
        cached: bool,
    ) -> Vec<Finding> {
        let max_tier = if cached { Tier::Cached } else { Tier::Quick };
        crate::doctor::facts::findings(&StatusFacts { targets, machine }, max_tier)
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

    #[test]
    fn every_attention_maps_to_its_failure_mode() {
        let attention = [
            Attention::JournalPending,
            Attention::StateVersionNewer,
            Attention::TargetMissing,
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
                (
                    ids::TARGET_VANISHED,
                    Severity::P2,
                    "The client's directory is gone",
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
        let forget = remediation(ids::TARGET_VANISHED);
        assert_eq!(forget.argv, argv(&["omaddons", "targets", "rm", "retail"]));
        assert!(forget.destructive);
        assert_eq!(
            forget.command.as_deref(),
            Some("omaddons targets rm retail")
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
    }

    /// A stuck journal names its state, and its remediation is its `doctor explain`, which
    /// parses; a settleable one keeps today's prose.
    #[test]
    fn stuck_rows_remediate_with_doctor_explain() {
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
            assert_eq!(argv, ["omaddons", "doctor", "explain", id, "-t", "retail"]);
            assert!(crate::cli::Cli::try_parse_from(&argv).is_ok(), "{argv:?}");
            assert!(!issue.remediation.destructive);
        }
        let machine = registry_machine_issues(None, &[unrecoverable(0, Unrecoverable::Unreadable)]);
        assert_eq!(
            machine[0].remediation.argv.as_deref(),
            Some(
                &["omaddons", "doctor", "explain", ids::JOURNAL_UNATTRIBUTED].map(str::to_owned)[..]
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

    // ---- Characterization (E1 part 1): what `status` emits today for every issue it
    // can produce, pinned in snapshots before the move onto the failure-mode registry.
    // Every case goes through `issues_of_case` or `machine_issues_of_case`; the move
    // rewires only their bodies, and must leave every snapshot byte-identical.

    /// A pending journal naming a target: its transaction, and why recovery cannot
    /// settle it by itself, when it cannot.
    pub(crate) type Journal = (TxId, Option<Unrecoverable>);

    /// One target as `status` reports it, with what the registry path will also read.
    pub(crate) struct Case {
        pub(crate) name: &'static str,
        pub(crate) target: StatusTarget,
        /// The pending journals naming the target, oldest first. Today's code reads only
        /// the first unrecoverable reason; the registry path reads one finding each.
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
        let all = [
            JournalPending,
            StateVersionNewer,
            TargetMissing,
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
            // `addon_degraded` attention for it.
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
                    t.missing = true;
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

    /// What `status` shows for a case: the registry's findings for its facts, one per
    /// journal, mapped to issues.
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
        let journals = Journals {
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
        let id = |k: TargetKey| k.to_string();
        let now: jiff::Timestamp = "2026-10-31T12:00:00Z".parse().unwrap();
        let free = journal_findings(&journals, &LockState::Free, &id, now);
        assert_eq!(free.len(), 2, "{free:?}");
        let held = LockState::Held {
            holder: Some(HolderInfo {
                pid: 7,
                command: "omaddons update".to_owned(),
                started_at: "2026-10-31T11:00:00Z".parse().unwrap(),
            }),
        };
        let found = journal_findings(&journals, &held, &id, now);
        let ids: Vec<&str> = found.iter().map(|f| f.id).collect();
        assert_eq!(
            ids,
            [ids::LOCK_HELD_LONG],
            "lock_held_long_is_a_machine_issue"
        );
        let issues = for_machine(&found);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].remediation.argv, None, "advice only");
        assert!(!JOURNAL_ROWS.contains(&issues[0].id), "never attention");
    }

    /// One target and the machine, as `status` builds them: the attention it raises from
    /// what it found, in its order, and the facts beside it.
    #[derive(Debug, Clone)]
    struct Generated {
        missing: bool,
        newer: bool,
        degraded: usize,
        drift: Vec<(&'static str, DriftState, u64)>,
        journals: Vec<Option<Unrecoverable>>,
        cached: bool,
        widget_outdated: bool,
        unattributed: Vec<Option<Unrecoverable>>,
    }

    impl Generated {
        /// The target `status` reports: under `--cached` only missing folders are drift;
        /// a missing target raises only `target_missing` (and a journal).
        fn target(&self) -> (StatusTarget, Vec<Journal>) {
            let journals: Vec<Journal> = self
                .journals
                .iter()
                .enumerate()
                .map(|(n, why)| (tx(n), *why))
                .collect();
            let mut t = test_target("retail", 0, &[], 0);
            t.key = Some(KEY.parse().unwrap());
            t.addons.degraded = self.degraded;
            let mut attention = Vec::new();
            if !journals.is_empty() {
                attention.push(Attention::JournalPending);
            }
            if self.missing {
                attention.push(Attention::TargetMissing);
                t.missing = true;
            } else {
                if self.newer {
                    attention.push(Attention::StateVersionNewer);
                }
                if self.degraded > 0 {
                    attention.push(Attention::AddonDegraded);
                }
                t.drift = self
                    .drift
                    .iter()
                    .filter(|(_, state, _)| !self.cached || *state == DriftState::Missing)
                    .map(|&(addon, state, files)| drift(addon, state, files))
                    .collect();
                if t.drift.iter().any(|d| d.state == DriftState::Modified) {
                    attention.push(Attention::AddonModified);
                }
                if t.drift.iter().any(|d| d.state == DriftState::Missing) {
                    attention.push(Attention::AddonFolderMissing);
                }
            }
            t.attention = attention;
            (t, journals)
        }

        fn widget(&self) -> WidgetInfo {
            WidgetInfo {
                omarchy: true,
                installed: true,
                enabled: true,
                version: Some("0.0.9".to_owned()),
                outdated: self.widget_outdated,
                context_matches: Some(true),
            }
        }

        fn unattributed(&self) -> Vec<Journal> {
            self.unattributed
                .iter()
                .enumerate()
                .map(|(n, why)| (tx(n), *why))
                .collect()
        }
    }

    fn unrecoverable_strategy() -> impl Strategy<Value = Option<Unrecoverable>> {
        prop_oneof![
            Just(None),
            Just(Some(Unrecoverable::TooNew)),
            Just(Some(Unrecoverable::Unreadable)),
        ]
    }

    fn generated() -> impl Strategy<Value = Generated> {
        let entry = (
            prop::sample::select(vec!["Aaa", "Details", "Plater", "Quiet"]),
            prop::sample::select(vec![
                DriftState::Missing,
                DriftState::Modified,
                DriftState::Unindexed,
            ]),
            0u64..4,
        );
        (
            any::<bool>(),
            any::<bool>(),
            0usize..4,
            prop::collection::vec(entry, 0..5),
            prop::collection::vec(unrecoverable_strategy(), 0..4),
            any::<bool>(),
            any::<bool>(),
            prop::collection::vec(unrecoverable_strategy(), 0..3),
        )
            .prop_map(
                |(
                    missing,
                    newer,
                    degraded,
                    drift,
                    journals,
                    cached,
                    widget_outdated,
                    unattributed,
                )| {
                    Generated {
                        missing,
                        newer,
                        degraded,
                        drift,
                        journals,
                        cached,
                        widget_outdated,
                        unattributed,
                    }
                },
            )
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(512))]

        /// Every issue but the journals' is today's: F1 names each journal state, so
        /// those differ from the legacy titles by design (`journal_*` tests pin them).
        #[test]
        fn the_registry_path_equals_the_legacy_path(g in generated()) {
            let not_journal = |issues: Vec<Issue>| -> serde_json::Value {
                let kept: Vec<Issue> = issues
                    .into_iter()
                    .filter(|i| !JOURNAL_ROWS.contains(&i.id))
                    .collect();
                serde_json::to_value(kept).unwrap()
            };
            let (t, journals) = g.target();
            let reason = journals
                .iter()
                .find_map(|(_, why)| why.map(Unrecoverable::reason));
            let legacy = not_journal(legacy::issues_for(&t, reason));
            let registry = not_journal(registry_issues(&t, &journals, g.cached));
            prop_assert_eq!(legacy, registry, "{:?}", g);

            let widget = g.widget();
            let unattributed = g.unattributed();
            let legacy = not_journal(legacy::machine_issues(
                Some(&widget),
                !unattributed.is_empty(),
            ));
            let registry = not_journal(registry_machine_issues(Some(&widget), &unattributed));
            prop_assert_eq!(legacy, registry, "{:?}", g);
        }
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
