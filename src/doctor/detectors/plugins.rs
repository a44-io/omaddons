//! `plugins`: the bar widget and the agent skill. Each row is reported, never repaired
//! (M4-D50, M4-D64): a widget reinstall runs Omarchy's own tools in the desktop session
//! and cannot be undone, so its remediation is `omaddons omarchy install`, the user's to
//! run; an outdated or edited skill is reinstalled with `robot-docs install-skill`, which
//! keeps an edited copy.

use std::path::Path;

use crate::contract::guide::EXPECTED_DIGEST;
use crate::contract::guide::stamp::{Stamp, hex, inspect};
use crate::doctor::budget::Budget;
use crate::doctor::finding::{Confidence, Detected, Evidence, Observation, Subject};
use crate::doctor::selection::Selection;
use crate::doctor::view::{BoundedRead, DoctorView, SkillFile};
use crate::omarchy::status::WidgetInfo;
use crate::omarchy::validate::{self, Checked};

/// `widget-version-drift`'s `evidence.reason`: the installed code differs from what this
/// binary renders for the directories it was installed under.
pub(crate) const CODE_DIFFERS: &str = "code_differs";
/// `widget-version-drift`'s `evidence.reason`: its `Build.js` cannot say which state it
/// serves.
pub(crate) const CONTEXT_UNKNOWN: &str = "context_unknown";
/// `widget-invalid`'s `evidence.reason`: the widget was installed under other `HOME` or
/// XDG directories, so it serves another omaddons state and nothing recommends its
/// reinstall from this run (M3-D7).
pub(crate) const OTHER_CONTEXT: &str = "other_context";

/// `skill-version-drift`'s `evidence.reason`: a pristine skill of another guide.
pub(crate) const SKILL_OUTDATED: &str = "outdated";
/// `skill-version-drift`'s `evidence.reason`: a skill whose bytes no longer match its own
/// stamp, which is not this build's.
pub(crate) const SKILL_EDITED: &str = "edited";
/// `skill-version-drift`'s `evidence.reason`: a `SKILL.md` without a guide stamp.
pub(crate) const SKILL_UNSTAMPED: &str = "unstamped";
/// `skill-version-drift`'s `evidence.reason`: a `SKILL.md` larger than any guide.
pub(crate) const SKILL_TOO_LARGE: &str = "too_large";
/// `skill-version-drift`'s `evidence.reason`: a symlinked `SKILL.md` whose target is not
/// this build's guide.
pub(crate) const SKILL_SYMLINK: &str = "symlink";
/// `skill-version-drift`'s `evidence.reason`: a `SKILL.md` that is not a regular file.
pub(crate) const SKILL_NOT_A_FILE: &str = "not_a_file";
/// `skill-version-drift`'s `evidence.reason`: a `SKILL.md` that cannot be read.
pub(crate) const SKILL_UNREADABLE: &str = "unreadable";

/// Every `evidence.reason` these checks give.
pub(crate) const REASONS: [&str; 10] = [
    CODE_DIFFERS,
    CONTEXT_UNKNOWN,
    OTHER_CONTEXT,
    SKILL_OUTDATED,
    SKILL_EDITED,
    SKILL_UNSTAMPED,
    SKILL_TOO_LARGE,
    SKILL_SYMLINK,
    SKILL_NOT_A_FILE,
    SKILL_UNREADABLE,
];

/// Why an installed skill was changed by hand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EditedCause {
    /// Its bytes no longer match its own stamp.
    Changed,
    /// It has no guide stamp.
    Unstamped,
    /// It is larger than any guide, so never read whole.
    TooLarge,
}

/// What an installed skill's `SKILL.md` (read through a symlink) is, judged by its stamp
/// against this build's guide digest, never by rendering the guide.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SkillState {
    /// No file there (a deleted skill, or a dangling link): never a finding.
    Absent,
    /// This build's guide, pristine or edited (an edited copy of the current guide is no
    /// finding: M4-D50 reports an edit only when its stamp also differs).
    Current,
    /// Another guide's pristine bytes.
    Outdated {
        digest: [u8; 32],
    },
    /// Changed by hand; `claimed` is its stamp when it has one.
    Edited {
        claimed: Option<[u8; 32]>,
        cause: EditedCause,
    },
    NotAFile,
    Unreadable,
}

/// `read`'s state against `expected`, this build's guide digest in lowercase hex.
pub(crate) fn classify(read: &BoundedRead, expected: &str) -> SkillState {
    match read {
        BoundedRead::Missing => SkillState::Absent,
        BoundedRead::Unreadable { kind } if *kind == std::io::ErrorKind::NotFound => {
            SkillState::Absent
        }
        BoundedRead::Unreadable { .. } => SkillState::Unreadable,
        BoundedRead::NotAFile => SkillState::NotAFile,
        BoundedRead::TooLarge { .. } => SkillState::Edited {
            claimed: None,
            cause: EditedCause::TooLarge,
        },
        BoundedRead::Bytes(bytes) => match inspect(bytes) {
            Stamp::Stamped { claimed, actual } => {
                let current = hex(&claimed).eq(expected);
                if current {
                    SkillState::Current
                } else if claimed == actual {
                    SkillState::Outdated { digest: actual }
                } else {
                    SkillState::Edited {
                        claimed: Some(claimed),
                        cause: EditedCause::Changed,
                    }
                }
            }
            Stamp::Unstamped => SkillState::Edited {
                claimed: None,
                cause: EditedCause::Unstamped,
            },
        },
    }
}

/// `skill-version-drift`: each installed agent skill (the default folder and every folder
/// the history projection lists) whose `SKILL.md` is not this build's guide. A missing
/// skill, or a link to nothing, is no finding; nothing is rendered or written.
pub(crate) fn skill_version_drift(view: &DoctorView, _: &Selection, _: &mut Budget) -> Detected {
    let found = view
        .skill()
        .unwrap_or_default()
        .iter()
        .filter_map(|skill| skill_observation(skill, EXPECTED_DIGEST))
        .collect();
    Detected::complete(found)
}

/// The finding for one installed skill, if it is one.
pub(crate) fn skill_observation(skill: &SkillFile, expected: &str) -> Option<Observation> {
    let state = classify(&skill.read, expected);
    let digest = |d: &[u8; 32]| format!("sha256:{}", hex(d));
    let (observed, edited) = match state {
        SkillState::Absent | SkillState::Current => return None,
        SkillState::Outdated { digest: d } => (Some(digest(&d)), Some(false)),
        SkillState::Edited { claimed, .. } => (claimed.as_ref().map(digest), Some(true)),
        SkillState::NotAFile | SkillState::Unreadable => (None, None),
    };
    let (title, reason) = match (&skill.link_target, state) {
        (Some(_), _) => (
            "The installed agent skill is a symbolic link to a file that is not this binary's guide",
            SKILL_SYMLINK,
        ),
        (None, SkillState::Outdated { .. }) => (
            "The installed agent skill carries another guide than this binary's",
            SKILL_OUTDATED,
        ),
        (
            None,
            SkillState::Edited {
                cause: EditedCause::Changed,
                ..
            },
        ) => (
            "The installed agent skill was edited by hand, and its guide is not this binary's",
            SKILL_EDITED,
        ),
        (
            None,
            SkillState::Edited {
                cause: EditedCause::Unstamped,
                ..
            },
        ) => (
            "The installed agent skill has no guide stamp, so it was changed by hand",
            SKILL_UNSTAMPED,
        ),
        (
            None,
            SkillState::Edited {
                cause: EditedCause::TooLarge,
                ..
            },
        ) => (
            "The installed agent skill is larger than any guide, so it was changed by hand",
            SKILL_TOO_LARGE,
        ),
        (None, SkillState::NotAFile) => (
            "The installed agent skill's SKILL.md is not a regular file",
            SKILL_NOT_A_FILE,
        ),
        (None, _) => ("The installed agent skill cannot be read", SKILL_UNREADABLE),
    };
    let mut evidence = Evidence {
        path: Some(skill.file.clone()),
        observed,
        expected: Some(format!("sha256:{expected}")),
        reason: Some(reason),
        edited,
        link_target: skill.link_target.clone(),
        ..Evidence::default()
    };
    if !evidence.is_private_safe() {
        evidence.link_target = None;
    }
    Some(Observation {
        subject: Subject::machine().path(&skill.file),
        title: title.to_owned(),
        confidence: Confidence::High,
        addon: None,
        folder: None,
        evidence,
    })
}

/// `widget-version-drift`, from the Widget layer: the rule's observation, with the plugin
/// folder it is about.
pub(crate) fn widget_version_drift(view: &DoctorView, _: &Selection, _: &mut Budget) -> Detected {
    let widget = view.widget().and_then(|layer| layer.info.as_ref());
    let mut found = widget_version_drift_in(widget);
    for obs in &mut found {
        obs.evidence.path = Some(view.paths().omarchy_plugin_dir());
    }
    Detected::complete(found)
}

/// `widget-version-drift`'s rule: the bar widget serving this state is from another build
/// (`status`'s `widget_outdated`). `observed` and `expected` can be equal (a rebuild of one
/// version), which is why `reason` says what differs.
pub(crate) fn widget_version_drift_in(widget: Option<&WidgetInfo>) -> Vec<Observation> {
    widget
        .filter(|widget| widget.needs_reinstall())
        .map(|widget| Observation {
            subject: Subject::machine(),
            title: "The bar widget is from another omaddons build".to_owned(),
            confidence: Confidence::High,
            addon: None,
            folder: None,
            evidence: Evidence {
                observed: widget.version.clone(),
                expected: Some(env!("CARGO_PKG_VERSION").to_owned()),
                // `Build.js` says which state it serves when `context_matches` is known.
                reason: Some(if widget.context_matches.is_some() {
                    CODE_DIFFERS
                } else {
                    CONTEXT_UNKNOWN
                }),
                ..Evidence::default()
            },
        })
        .into_iter()
        .collect()
}

/// `widget-invalid`: the installed widget breaks Omarchy's plugin rules, as omaddons'
/// port of its validator checks them. The validator's walk is charged one entry per entry
/// it visits, so a walk the budget stops is reported cut short, never clean.
pub(crate) fn widget_invalid(view: &DoctorView, _: &Selection, budget: &mut Budget) -> Detected {
    let Some(info) = view.widget().and_then(|layer| layer.info.as_ref()) else {
        return Detected::complete(Vec::new());
    };
    // No shell, or no widget: nothing for Omarchy to load.
    if !(info.omarchy && info.installed) {
        return Detected::complete(Vec::new());
    }
    let dir = view.paths().omarchy_plugin_dir();
    let checked = {
        let mut charge = || budget.charge(1).is_ok();
        validate::check(&dir, &mut charge)
    };
    let found = widget_invalid_in(&dir, info, &checked)
        .into_iter()
        .collect();
    if checked.complete {
        Detected::complete(found)
    } else {
        Detected::cut_short(found, budget)
    }
}

/// `widget-invalid`'s observation of what the validator found: the rule codes and their
/// count, never a message, since messages quote manifest values (M4-D42).
fn widget_invalid_in(dir: &Path, info: &WidgetInfo, checked: &Checked) -> Option<Observation> {
    let count = checked.problems.len();
    if count == 0 {
        return None;
    }
    let noun = if count == 1 { "problem" } else { "problems" };
    Some(Observation {
        subject: Subject::machine(),
        title: format!("The bar widget fails Omarchy's plugin validation ({count} {noun})"),
        confidence: Confidence::High,
        addon: None,
        folder: None,
        evidence: Evidence {
            path: Some(dir.to_path_buf()),
            count: Some(count as u64),
            rules: checked.rules(),
            reason: (info.context_matches == Some(false)).then_some(OTHER_CONTEXT),
            ..Evidence::default()
        },
    })
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::symlink;

    use serde_json::{Value, json};

    use super::*;
    use crate::app::issues;
    use crate::doctor::finding::Finding;
    use crate::doctor::registry::{self, ids};
    use crate::doctor::view::tests::home;
    use crate::doctor::view::{LayerState, Preloaded, WidgetLayer};
    use crate::omarchy::render::{Context, render};
    use crate::omarchy::validate::Rule;
    use crate::state::paths::Paths;

    /// A widget layer: Omarchy's shell, installed, outdated and context as given.
    fn info(omarchy: bool, installed: bool, outdated: bool, context: Option<bool>) -> WidgetInfo {
        WidgetInfo {
            omarchy,
            installed,
            enabled: installed,
            version: installed.then(|| "0.0.9".to_owned()),
            outdated,
            context_matches: context,
        }
    }

    fn view_of(paths: &Paths, info: Option<WidgetInfo>) -> DoctorView {
        DoctorView::preloaded(
            paths.clone(),
            Preloaded {
                widget: Some(LayerState::Available(WidgetLayer { info })),
                ..Preloaded::default()
            },
        )
    }

    /// A small document stamped as the renderer stamps the guide: another guide than this
    /// build's.
    fn guide_doc(body: &str) -> Vec<u8> {
        use crate::contract::guide::stamp::{digest_without_stamp, line};
        let zeros = "0".repeat(64);
        let provisional = format!("---\nname: omaddons\n---\n{}\n{body}", line(&zeros));
        let digest = digest_without_stamp(&provisional).unwrap();
        provisional.replacen(&zeros, &hex(&digest), 1).into_bytes()
    }

    /// `bytes` with its second-to-last byte changed.
    fn edited(mut bytes: Vec<u8>) -> Vec<u8> {
        let last = bytes.len() - 2;
        bytes[last] ^= 0x01;
        bytes
    }

    #[test]
    fn skill_states_classify_by_stamp() {
        let doc = guide_doc("a guide\n");
        let digest = crate::contract::guide::stamp::digest_bytes_without_stamp(&doc).unwrap();
        let this = hex(&digest);
        let other = "1".repeat(64);
        let cases: Vec<(&str, BoundedRead, &str, SkillState)> = vec![
            ("missing", BoundedRead::Missing, &this, SkillState::Absent),
            (
                "dangling_link",
                BoundedRead::Unreadable {
                    kind: std::io::ErrorKind::NotFound,
                },
                &this,
                SkillState::Absent,
            ),
            (
                "this_guide",
                BoundedRead::Bytes(doc.clone()),
                &this,
                SkillState::Current,
            ),
            (
                "this_guide_edited",
                BoundedRead::Bytes(edited(doc.clone())),
                &this,
                SkillState::Current,
            ),
            (
                "another_guide",
                BoundedRead::Bytes(doc.clone()),
                &other,
                SkillState::Outdated { digest },
            ),
            (
                "another_guide_edited",
                BoundedRead::Bytes(edited(doc.clone())),
                &other,
                SkillState::Edited {
                    claimed: Some(digest),
                    cause: EditedCause::Changed,
                },
            ),
            (
                "unstamped",
                BoundedRead::Bytes(b"# my notes\n".to_vec()),
                &other,
                SkillState::Edited {
                    claimed: None,
                    cause: EditedCause::Unstamped,
                },
            ),
            (
                "too_large",
                BoundedRead::TooLarge { bytes: 2 << 20 },
                &other,
                SkillState::Edited {
                    claimed: None,
                    cause: EditedCause::TooLarge,
                },
            ),
            (
                "not_a_file",
                BoundedRead::NotAFile,
                &other,
                SkillState::NotAFile,
            ),
            (
                "unreadable",
                BoundedRead::Unreadable {
                    kind: std::io::ErrorKind::PermissionDenied,
                },
                &other,
                SkillState::Unreadable,
            ),
        ];
        for (case, read, expected, want) in cases {
            let got = classify(&read, expected);
            assert_eq!(got, want, "{case}: against {expected} → {got:?}");
        }
    }

    /// Through the view: the default folder and each folder the history projection lists,
    /// each finding about its own file; a listed folder that is gone is none.
    #[test]
    fn skill_findings_name_each_installed_folder() {
        use crate::fsx::{ScopeTable, WriteSession, Writer};
        use crate::model::{Actor, TxKind};
        use crate::state::history::{self, SettingEntry};
        let (_dir, paths) = home();
        let home = paths.home().to_path_buf();
        let old = guide_doc("an older guide\n");
        let default = paths.skill_dir();
        fs::create_dir_all(&default).unwrap();
        fs::write(default.join("SKILL.md"), &old).unwrap();
        let custom = home.join("work/skills/omaddons");
        fs::create_dir_all(&custom).unwrap();
        fs::write(custom.join("SKILL.md"), edited(old.clone())).unwrap();
        let linked = home.join("dots/skills/omaddons");
        fs::create_dir_all(&linked).unwrap();
        fs::write(home.join("dots/guide.md"), &old).unwrap();
        symlink(home.join("dots/guide.md"), linked.join("SKILL.md")).unwrap();
        let gone = home.join("gone/skills/omaddons");

        fs::create_dir_all(paths.state_dir()).unwrap();
        let scopes = ScopeTable::new(&paths, &[], None).unwrap();
        let mut s = WriteSession::untracked(Writer::Mutating, &scopes, "test");
        for dir in [&gone, &linked, &custom] {
            let entry = SettingEntry {
                dir: Some(dir.clone()),
                ..SettingEntry::now(TxKind::Skill, None, Actor::Human, None, vec![])
            };
            history::append_setting(&mut s, &paths, &entry).unwrap();
        }
        crate::state::history_tail::rebuild_if_stale(&paths, "test", Actor::Human);

        let view = crate::doctor::view::tests::view_at(paths.clone());
        let detected = skill_version_drift(&view, &Selection::all(), &mut Budget::unlimited());
        let found = findings(ids::SKILL_VERSION_DRIFT, &detected);
        let digest = format!(
            "sha256:{}",
            hex(&crate::contract::guide::stamp::digest_bytes_without_stamp(&old).unwrap())
        );
        let by_path = |dir: &std::path::Path| {
            let file = dir.join("SKILL.md");
            found
                .iter()
                .find(|f| f.evidence.path.as_deref() == Some(file.as_path()))
                .unwrap_or_else(|| panic!("no finding about {}: {found:#?}", file.display())) // ubs:ignore — the test's own failure
        };
        assert_eq!(found.len(), 3, "{found:#?}");
        let outdated = by_path(&default);
        assert_eq!(
            (
                outdated.evidence.reason,
                outdated.evidence.edited,
                outdated.evidence.observed.as_deref()
            ),
            (Some(SKILL_OUTDATED), Some(false), Some(digest.as_str()))
        );
        // The remediation omits --dir only for this process's own default folder (the
        // registry's test covers it); this temporary home is not the process's HOME.
        assert_eq!(
            outdated.remediation.argv,
            Some(
                ["omaddons", "robot-docs", "install-skill", "--dir"]
                    .map(str::to_owned)
                    .into_iter()
                    .chain([default.display().to_string()])
                    .collect()
            ),
            "{:?}",
            outdated.remediation
        );
        let changed = by_path(&custom);
        assert_eq!(
            (changed.evidence.reason, changed.evidence.edited),
            (Some(SKILL_EDITED), Some(true))
        );
        assert!(
            changed.remediation.argv.as_ref().is_some_and(
                |argv| argv.ends_with(&["--dir".to_owned(), custom.display().to_string()])
            ),
            "{:?}",
            changed.remediation
        );
        let link = by_path(&linked);
        assert_eq!(
            (
                link.evidence.reason,
                link.evidence.edited,
                link.evidence.link_target.as_deref()
            ),
            (
                Some(SKILL_SYMLINK),
                Some(false),
                Some(home.join("dots/guide.md").as_path())
            )
        );
        assert_eq!(link.remediation.argv, None, "a link gets prose");
        for finding in &found {
            assert_eq!(finding.severity, crate::doctor::finding::Severity::P3);
            assert!(finding.evidence.is_private_safe(), "{:?}", finding.evidence);
        }
    }

    fn findings(id: &str, detected: &Detected) -> Vec<Finding> {
        let row = registry::row(id).unwrap();
        detected
            .found
            .iter()
            .cloned()
            .map(|obs| row.finding(obs))
            .collect()
    }

    #[test]
    fn widget_drift_cases() {
        let (_dir, paths) = home();
        let plugin_dir = paths.omarchy_plugin_dir();
        let version = env!("CARGO_PKG_VERSION");
        // (case, layer, the evidence's reason when found)
        let cases: [(&str, Option<WidgetInfo>, Option<&str>); 6] = [
            ("no_omarchy", None, None),
            ("not_installed", Some(info(true, false, false, None)), None),
            (
                "outdated_known_context",
                Some(info(true, true, true, Some(true))),
                Some(CODE_DIFFERS),
            ),
            (
                "outdated_unknown_context",
                Some(info(true, true, true, None)),
                Some(CONTEXT_UNKNOWN),
            ),
            (
                "outdated_other_state",
                Some(info(true, true, true, Some(false))),
                None,
            ),
            ("current", Some(info(true, true, false, Some(true))), None),
        ];
        for (case, layer, reason) in cases {
            let view = view_of(&paths, layer.clone());
            let detected = widget_version_drift(&view, &Selection::all(), &mut Budget::unlimited());
            assert_eq!(detected.incomplete, None, "{case}");
            let found = findings(ids::WIDGET_VERSION_DRIFT, &detected);
            let Some(reason) = reason else {
                assert_eq!(found, Vec::new(), "{case}: {layer:?}");
                continue;
            };
            assert_eq!(found.len(), 1, "{case}: {layer:?} gave {found:?}");
            let f = &found[0];
            assert_eq!(
                f.evidence,
                Evidence {
                    path: Some(plugin_dir.clone()),
                    observed: Some("0.0.9".to_owned()),
                    expected: Some(version.to_owned()),
                    reason: Some(reason),
                    ..Evidence::default()
                },
                "{case}: {layer:?}"
            );
            assert_eq!(
                f.instance.as_str(),
                format!("{}@machine", ids::WIDGET_VERSION_DRIFT)
            );
            assert_eq!(
                f.remediation.argv,
                Some(vec!["omaddons".into(), "omarchy".into(), "install".into()]),
                "{case}"
            );
            assert!(!f.remediation.auto_fixable, "{case}");
        }
    }

    /// The rule gives the issue `status` printed before the doctor gave it evidence, and
    /// nothing for a current widget.
    #[test]
    fn drift_issue_matches_status() {
        let row = registry::row(ids::WIDGET_VERSION_DRIFT).unwrap();
        let expected = issues::Issue {
            id: ids::WIDGET_VERSION_DRIFT,
            severity: issues::Severity::P3,
            title: "The bar widget is from another omaddons build".to_owned(),
            addon: None,
            remediation: issues::Remediation {
                command: Some("omaddons omarchy install".to_owned()),
                argv: Some(vec!["omaddons".into(), "omarchy".into(), "install".into()]),
                destructive: false,
            },
            auto_fixable: false,
            explain: issues::ExplainCommand {
                command: format!("omaddons doctor explain {}", ids::WIDGET_VERSION_DRIFT),
                argv: vec![
                    "omaddons".into(),
                    "doctor".into(),
                    "explain".into(),
                    ids::WIDGET_VERSION_DRIFT.into(),
                ],
            },
        };
        for (case, outdated, context) in [
            ("outdated_context_known", true, Some(true)),
            ("outdated_context_unknown", true, None),
            ("current", false, Some(true)),
        ] {
            let info = info(true, true, outdated, context);
            let found: Vec<Finding> = widget_version_drift_in(Some(&info))
                .into_iter()
                .map(|obs| row.finding(obs))
                .collect();
            let want = if outdated {
                vec![expected.clone()]
            } else {
                Vec::new()
            };
            assert_eq!(issues::for_machine(&found), want, "{case}: {info:?}");
        }
    }

    /// Writes this binary's rendered widget into `dir`.
    fn write_rendered(dir: &Path) {
        let ctx = Context {
            home: "/h".into(),
            xdg_config_home: "/h/.config".into(),
            xdg_state_home: "/h/.local/state".into(),
            xdg_cache_home: "/h/.cache".into(),
            xdg_data_home: "/h/.local/share".into(),
        };
        let rendered = render(Path::new("/o/omaddons"), &ctx, "a44.omaddons").unwrap();
        for (path, bytes) in &rendered.files {
            let target = dir.join(path);
            fs::create_dir_all(target.parent().unwrap()).unwrap();
            fs::write(target, bytes).unwrap();
        }
    }

    /// The rendered widget's manifest, edited.
    fn edit_manifest(dir: &Path, edit: impl FnOnce(&mut Value)) {
        let path = dir.join("manifest.json");
        let mut manifest: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        edit(&mut manifest);
        fs::write(&path, manifest.to_string()).unwrap();
    }

    /// What a case plants in the plugin folder.
    enum Tree {
        Nothing,
        Rendered,
        OneRule,
        TwoRules,
    }

    #[test]
    fn widget_invalid_cases() {
        let install = ["omaddons", "omarchy", "install"]
            .map(str::to_owned)
            .to_vec();
        // (case, layer, plugin folder, (count, rules, reason) when found, argv)
        type Expected = Option<(u64, Vec<Rule>, Option<&'static str>, Option<Vec<String>>)>;
        let cases: Vec<(&str, Option<WidgetInfo>, Tree, Expected)> = vec![
            ("no_omarchy", None, Tree::TwoRules, None),
            (
                "config_without_shell",
                Some(info(false, true, false, Some(true))),
                Tree::TwoRules,
                None,
            ),
            (
                "not_installed",
                Some(info(true, false, false, None)),
                Tree::Nothing,
                None,
            ),
            (
                "rendered_widget",
                Some(info(true, true, false, Some(true))),
                Tree::Rendered,
                None,
            ),
            (
                "one_rule",
                Some(info(true, true, false, Some(true))),
                Tree::OneRule,
                Some((1, vec![Rule::DefaultSection], None, Some(install.clone()))),
            ),
            (
                "two_rules",
                Some(info(true, true, false, Some(true))),
                Tree::TwoRules,
                Some((
                    2,
                    vec![Rule::DefaultSection, Rule::Symlink],
                    None,
                    Some(install.clone()),
                )),
            ),
            (
                "unknown_context",
                Some(info(true, true, true, None)),
                Tree::OneRule,
                Some((1, vec![Rule::DefaultSection], None, Some(install.clone()))),
            ),
            (
                "another_state",
                Some(info(true, true, false, Some(false))),
                Tree::TwoRules,
                Some((
                    2,
                    vec![Rule::DefaultSection, Rule::Symlink],
                    Some(OTHER_CONTEXT),
                    None,
                )),
            ),
        ];
        for (case, layer, tree, expected) in cases {
            let (_dir, paths) = home();
            let plugin_dir = paths.omarchy_plugin_dir();
            match tree {
                Tree::Nothing => {}
                Tree::Rendered => write_rendered(&plugin_dir),
                Tree::OneRule => {
                    write_rendered(&plugin_dir);
                    edit_manifest(&plugin_dir, |m| {
                        m["barWidget"]["defaultSection"] = json!("top")
                    });
                }
                Tree::TwoRules => {
                    write_rendered(&plugin_dir);
                    edit_manifest(&plugin_dir, |m| {
                        m["barWidget"]["defaultSection"] = json!("top")
                    });
                    fs::create_dir_all(plugin_dir.join("views")).unwrap();
                    symlink("../manifest.json", plugin_dir.join("views/link")).unwrap();
                }
            }
            let view = view_of(&paths, layer.clone());
            let detected = widget_invalid(&view, &Selection::all(), &mut Budget::unlimited());
            assert_eq!(detected.incomplete, None, "{case}");
            let found = findings(ids::WIDGET_INVALID, &detected);
            let Some((count, rules, reason, argv)) = expected else {
                assert_eq!(found, Vec::new(), "{case}: {layer:?}");
                continue;
            };
            assert_eq!(found.len(), 1, "{case}: {layer:?} gave {found:?}");
            let f = &found[0];
            assert_eq!(
                f.evidence,
                Evidence {
                    path: Some(plugin_dir.clone()),
                    count: Some(count),
                    rules,
                    reason,
                    ..Evidence::default()
                },
                "{case}"
            );
            let noun = if count == 1 { "problem" } else { "problems" };
            assert_eq!(
                f.title,
                format!("The bar widget fails Omarchy's plugin validation ({count} {noun})"),
                "{case}"
            );
            assert_eq!(f.remediation.argv, argv, "{case}: {:?}", f.remediation);
            assert!(!f.remediation.auto_fixable, "{case}");
            assert!(!f.remediation.destructive, "{case}");
            // Never a validator message, which quotes manifest values.
            let wire = serde_json::to_string(f).unwrap();
            assert!(!wire.contains("top"), "{case}: {wire}");
            assert!(!wire.contains("must be"), "{case}: {wire}");
        }
    }

    /// A budget of one entry over a deep tree: what was found before the cut stands, and
    /// the check reports the limit, never a clean result.
    #[test]
    fn a_cut_short_walk_keeps_its_findings() {
        let (_dir, paths) = home();
        let plugin_dir = paths.omarchy_plugin_dir();
        write_rendered(&plugin_dir);
        edit_manifest(&plugin_dir, |m| {
            m["barWidget"]["defaultSection"] = json!("top")
        });
        fs::create_dir_all(plugin_dir.join("a/b/c/d/e")).unwrap();
        let view = view_of(&paths, Some(info(true, true, false, Some(true))));
        let mut budget = Budget::new(std::time::Duration::from_secs(60), 1);
        let detected = widget_invalid(&view, &Selection::all(), &mut budget);
        let incomplete = detected.incomplete.expect("cut short");
        assert_eq!(
            incomplete.limit,
            crate::doctor::finding::IncompleteLimit::Entries,
            "{detected:?}"
        );
        let found = findings(ids::WIDGET_INVALID, &detected);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].evidence.rules, vec![Rule::DefaultSection]);
    }
}
