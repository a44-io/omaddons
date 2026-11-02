//! `plugins`: the bar widget. Both rows are reported, never repaired (M4-D50): a
//! reinstall runs Omarchy's own tools in the desktop session and cannot be undone, so the
//! remediation is `omaddons omarchy install`, the user's to run.

use std::path::Path;

use crate::app::status::machine_facts;
use crate::doctor::budget::Budget;
use crate::doctor::facts::StatusFacts;
use crate::doctor::finding::{Confidence, Detected, Evidence, Observation, Subject};
use crate::doctor::selection::Selection;
use crate::doctor::view::DoctorView;
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

/// Every `evidence.reason` these checks give.
pub(crate) const REASONS: [&str; 3] = [CODE_DIFFERS, CONTEXT_UNKNOWN, OTHER_CONTEXT];

/// `widget-version-drift`, from the Widget layer: the status rule's observation, with the
/// plugin folder it is about.
pub(crate) fn widget_version_drift(view: &DoctorView, _: &Selection, _: &mut Budget) -> Detected {
    let widget = view.widget().and_then(|layer| layer.info.as_ref());
    let facts = StatusFacts {
        targets: Vec::new(),
        machine: machine_facts(widget),
    };
    let mut found = widget_version_drift_in(&facts);
    for obs in &mut found {
        obs.evidence.path = Some(view.paths().omarchy_plugin_dir());
    }
    Detected::complete(found)
}

/// `widget-version-drift`'s rule, which `status` runs too: the bar widget serving this
/// state is from another build. `observed` and `expected` can be equal (a rebuild of one
/// version), which is why `reason` says what differs.
pub(crate) fn widget_version_drift_in(facts: &StatusFacts) -> Vec<Observation> {
    facts
        .machine
        .widget
        .iter()
        .map(|widget| Observation {
            subject: Subject::machine(),
            title: "The bar widget is from another omaddons build".to_owned(),
            confidence: Confidence::High,
            addon: None,
            folder: None,
            evidence: Evidence {
                observed: widget.version.clone(),
                expected: Some(env!("CARGO_PKG_VERSION").to_owned()),
                reason: Some(if widget.context_known {
                    CODE_DIFFERS
                } else {
                    CONTEXT_UNKNOWN
                }),
                ..Evidence::default()
            },
        })
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

    /// The rule `status` runs gives the issue `status` printed before the doctor gave it
    /// evidence.
    #[test]
    fn drift_issue_matches_status() {
        let row = registry::row(ids::WIDGET_VERSION_DRIFT).unwrap();
        for context in [Some(true), None] {
            let info = info(true, true, true, context);
            let facts = StatusFacts {
                targets: Vec::new(),
                machine: machine_facts(Some(&info)),
            };
            let found: Vec<Finding> = widget_version_drift_in(&facts)
                .into_iter()
                .map(|obs| row.finding(obs))
                .collect();
            assert_eq!(
                issues::for_machine(&found),
                issues::legacy::machine_issues(Some(&info), false),
                "{info:?}"
            );
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
