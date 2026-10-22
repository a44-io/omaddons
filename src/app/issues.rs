//! `status`'s issues (M3-D37): each attention item as a finding shaped like the M4
//! doctor's, with vision D20's failure-mode id, a P0–P3 severity, a title and a
//! remediation, so the bar widget and agents read one ranked list instead of
//! interpreting attention codes (vision D2: the view holds no product logic). The
//! remediations come from the same hints as `status`'s `commands`, so the two never
//! disagree. Pure: computed from a target as `status` reports it.

use schemars::JsonSchema;
use serde::Serialize;

use super::drift::reinstall_hint;
use super::status::{Attention, StatusTarget, forget_target_hint, inspect_hint};
use crate::drift::DriftState;
use crate::error::CommandHint;

/// The failure-mode ids (vision D20), one table the M4 doctor imports.
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
}

/// The `journal_pending` reasons a recovery cannot settle by itself: the change needs
/// manual repair.
const MANUAL_REPAIR: &[&str] = &[
    "unexpected_state",
    "journal_unreadable",
    "journal_too_new",
    "preservation_incomplete",
];

/// How urgent an issue is (vision D20): the bar turns urgent on P0 and P1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, JsonSchema)]
pub enum Severity {
    P0,
    P1,
    P2,
    P3,
}

/// What addresses an issue: a command (`argv`), or prose (`argv` `null`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Remediation {
    /// The command for display, or the prose when there is no command.
    pub command: Option<String>,
    pub argv: Option<Vec<String>>,
    pub destructive: bool,
}

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

/// `target`'s issues, ranked by severity, then id, then addon. `journal_reason` is why a
/// pending journal cannot be recovered by itself, when it cannot (`unexpected_state`,
/// `journal_unreadable`, `journal_too_new`, `preservation_incomplete`).
pub fn issues_for(target: &StatusTarget, journal_reason: Option<&str>) -> Vec<Issue> {
    let mut issues = Vec::new();
    for attention in &target.attention {
        match attention {
            Attention::JournalPending => {
                let manual = journal_reason.is_some_and(|r| MANUAL_REPAIR.contains(&r));
                let (title, remediation) = if manual {
                    (
                        "An interrupted change needs manual repair",
                        "see \"journal_pending that does not clear\" under Troubleshooting in the README",
                    )
                } else {
                    (
                        "An interrupted change waits to be finished",
                        "quit the game if it is running, then run any change command; it finishes or rolls back the change first",
                    )
                };
                issues.push(Issue {
                    id: ids::JOURNAL_PENDING,
                    severity: Severity::P0,
                    title: title.to_owned(),
                    addon: None,
                    remediation: Remediation::prose(remediation),
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
mod tests {
    use super::*;
    use crate::app::drift::DriftItem;
    use crate::app::status::test_target;

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
        let issues = issues_for(&t, None);
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
        for prose in [ids::JOURNAL_PENDING, ids::STATE_VERSION_NEWER] {
            assert_eq!(remediation(prose).argv, None, "{prose}");
            assert!(remediation(prose).command.is_some(), "{prose}");
        }
    }

    #[test]
    fn a_journal_that_needs_manual_repair_says_so() {
        let t = test_target("retail", 0, &[Attention::JournalPending], 0);
        for reason in [
            "unexpected_state",
            "journal_unreadable",
            "journal_too_new",
            "preservation_incomplete",
        ] {
            let issues = issues_for(&t, Some(reason));
            assert_eq!(
                issues[0].title, "An interrupted change needs manual repair",
                "{reason}"
            );
            assert_eq!(issues[0].remediation.argv, None);
        }
        let other = issues_for(&t, Some("something_else"));
        assert_eq!(other[0].title, "An interrupted change waits to be finished");
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
        let titles: Vec<String> = issues_for(&t, None).into_iter().map(|i| i.title).collect();
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
        counts.add(&issues_for(&t, None));
        assert_eq!((counts.p0, counts.p2, counts.total()), (1, 1, 2));
        assert!(counts.urgent());
    }
}
