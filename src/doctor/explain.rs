//! `doctor explain` (E3; M4-D30, M4-D33, M4-D34): one failure mode in full, and its live
//! instances. It re-runs that one check the way a diagnose would, under the full detector
//! budget whatever its tier, and writes nothing: no run directory, no request.

use schemars::JsonSchema;
use serde::Serialize;

use super::budget::Budget;
use super::finding::{Finding, Incomplete, InstanceKey, Severity, Subsystem};
use super::registry::{self, FailureMode, ScopeName, Tier};
use super::runtime::{Checked, run_check};
use super::selection::{
    Selection, SkipReason, Skipped, closest_id, partition, target_skips, visible_failures,
};
use super::view::{DoctorView, merge_findings};
use crate::cli::{command_path, fix_argv_replacing};
use crate::engine::repair_action::FixKind;
use crate::error::{Error, ErrorCode, Result, Suggestion};
use crate::limits::{DOCTOR_DETECTOR_DEADLINE, DOCTOR_SCAN_MAX_ENTRIES};

/// What `<ID>` asked for: a failure mode, and one of its instances when a key was given.
#[derive(Debug, Clone)]
pub struct Asked {
    pub row: &'static FailureMode,
    /// The instance key, in its canonical form.
    pub instance: Option<String>,
}

/// `<ID>` read as an instance key when it holds `@`, else as a failure-mode id. An unknown
/// id, or a malformed key, is `usage`: the fix corrects the id in place to the registered
/// one within two edits, else it is the command's help.
pub fn resolve(given: &str, argv: &[String]) -> Result<Asked> {
    let Some((fm, _)) = given.split_once('@') else {
        return match registry::row(given) {
            Some(row) => Ok(Asked {
                row,
                instance: None,
            }),
            None => Err(unknown(given, given, argv)),
        };
    };
    match InstanceKey::parse(given) {
        Ok(parsed) => match registry::row(&parsed.fm) {
            Some(row) => Ok(Asked {
                row,
                instance: Some(parsed.to_string()),
            }),
            None => Err(unknown(given, fm, argv)),
        },
        Err(super::finding::InstanceParseError::BadFmId(_)) => Err(unknown(given, fm, argv)),
        Err(err) => Err(usage(
            format!("'{given}' is not an instance key: {err}"),
            None,
            argv,
        )),
    }
}

/// `usage` for `given`, whose failure-mode part `fm` is not a registered id.
fn unknown(given: &str, fm: &str, argv: &[String]) -> Error {
    let corrected = closest_id(fm).map(|(_, id)| format!("{id}{}", &given[fm.len()..]));
    usage(
        format!("unknown failure mode '{fm}'"),
        corrected.and_then(|corrected| fix_argv_replacing(argv, given, &corrected)),
        argv,
    )
}

/// `usage` whose fix is `fix`, keeping the user's verb, else the command's help.
fn usage(message: String, fix: Option<Vec<String>>, argv: &[String]) -> Error {
    match fix {
        Some(fix) => Error::new(ErrorCode::Usage, message, Suggestion::command(fix)).user_verb(),
        None => {
            let help: Vec<String> = ["omaddons", "help"]
                .into_iter()
                .map(str::to_owned)
                .chain(command_path(argv))
                .collect();
            Error::new(ErrorCode::Usage, message, Suggestion::command(help))
        }
    }
}

/// `doctor explain`'s report (M4-D34, §6.11), at the doctor envelope's top level.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct ExplainReport {
    pub failure_mode: ExplainedFailureMode,
    /// Its live instances, the same shape as `doctor diagnose`'s findings: every one, or
    /// the one the instance key names; `-t` narrows. Empty when none is there now.
    pub instances: Vec<Finding>,
    /// Where it could not be checked, as `doctor diagnose` says it: a layer it reads that
    /// could not be loaded, for every client or one, or the network it needs.
    pub skipped: Vec<Skipped>,
    /// The limit that stopped its check, so `instances` may be partial; `null` when the
    /// check ran to its end.
    pub incomplete: Option<Incomplete>,
}

/// A failure mode as the registry holds it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct ExplainedFailureMode {
    pub id: &'static str,
    pub subsystem: Subsystem,
    pub severity: Severity,
    pub title: &'static str,
    pub description: &'static str,
    pub why: &'static str,
    pub tier: Tier,
    /// What `doctor fix` would do about it; `null` when no fixer repairs it.
    pub fixer: Option<ExplainedFixer>,
    /// What a person does about it; `null` when the fixer alone repairs it.
    pub manual: Option<ExplainedManual>,
}

/// A failure mode's fixer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct ExplainedFixer {
    pub ops: Vec<FixKind>,
    /// The scopes its writes are confined to.
    pub scopes: Vec<ScopeName>,
    /// It runs by default; a fixer that changes user content ships disabled.
    pub enabled: bool,
    /// It runs only with `--force`.
    pub needs_force: bool,
}

/// A failure mode's manual remediation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct ExplainedManual {
    /// A value only you have is a `<placeholder>`.
    pub instruction: &'static str,
    /// Why it is yours to do.
    pub why: &'static str,
}

impl ExplainedFailureMode {
    pub fn of(row: &FailureMode) -> Self {
        Self {
            id: row.id,
            subsystem: row.subsystem,
            severity: row.severity,
            title: row.title,
            description: row.description,
            why: row.why,
            tier: row.tier,
            fixer: row.fix.map(|fix| ExplainedFixer {
                ops: fix.ops.to_vec(),
                scopes: fix.writes_to.to_vec(),
                enabled: fix.enabled,
                needs_force: fix.needs_force,
            }),
            manual: row.manual.map(|manual| ExplainedManual {
                instruction: manual.instruction,
                why: manual.why,
            }),
        }
    }
}

/// `asked`'s report over `view`, for the clients `target` (`-t`) names: its check run once
/// as a diagnose of that row would, under the full detector budget. A runtime row has no
/// check (its instances live in a run's report), and one that needs the network is never
/// run (explain makes no request). Fails when `-t` names no client, and when the check
/// panics: explain has no run to go on with, and an answer without its check would be
/// wrong.
pub fn explain(view: &DoctorView, asked: &Asked, target: Option<&str>) -> Result<ExplainReport> {
    let row = asked.row;
    let selection = Selection {
        target: target.map(str::to_owned),
        ..Selection::only(&[row.id])
    };
    let part = partition(view, &selection, std::slice::from_ref(row), &[row.tier])?;
    let mut skipped = part.skipped.clone();
    let mut detected = Vec::new();
    let mut incomplete = None;
    if row.online {
        skipped.push(Skipped {
            id: row.id,
            reason: SkipReason::OnlineRequired,
            target: None,
        });
    } else if let Some(detect) = row.detect.filter(|_| !part.run.is_empty()) {
        let mut budget = Budget::new(DOCTOR_DETECTOR_DEADLINE, DOCTOR_SCAN_MAX_ENTRIES);
        match run_check(row, detect, view, &selection, &mut budget) {
            Checked::Ran { findings, cut } => {
                detected = findings;
                incomplete = cut;
            }
            Checked::Crashed(location) => return Err(crashed(row, location)),
        }
    }
    skipped.extend(target_skips(view, &part));
    let failures = visible_failures(view, &selection, &part, &skipped);
    let instances = merge_findings(failures, detected)
        .into_iter()
        .filter(|f| f.id == row.id && part.scope.includes(f.target.as_ref()))
        .filter(|f| {
            asked
                .instance
                .as_deref()
                .is_none_or(|key| f.instance.as_str() == key)
        })
        .collect();
    skipped.retain(|s| s.id == row.id);
    Ok(ExplainReport {
        failure_mode: ExplainedFailureMode::of(row),
        instances,
        skipped,
        incomplete,
    })
}

/// `internal` for a check that panicked, at `location` when the panic hook saw it.
fn crashed(row: &FailureMode, location: Option<String>) -> Error {
    let at = location.map_or_else(String::new, |location| format!(" at {location}"));
    Error::new(
        ErrorCode::Internal,
        format!(
            "the detector of {}{at} panicked; this is a bug in omaddons",
            row.id
        ),
        Suggestion::text("report the failure mode and the location in this message"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doctor::registry::{FAILURE_MODES, ids};

    const K: &str = "01M5ZQ7Y3KJ8V9W2X4C6B0N1RT";

    fn argv(words: &[&str]) -> Vec<String> {
        std::iter::once("omaddons")
            .chain(words.iter().copied())
            .map(str::to_owned)
            .collect()
    }

    #[test]
    fn instance_key_parsing() {
        let fm = ids::JOURNAL_PENDING;
        let tx = "01M7A1B2C3D4E5F6G7H8J9K0MN";
        let ok = [
            ("machine_no_discriminator", format!("{fm}@machine")),
            ("target_and_tx", format!("{fm}@{K}:tx={tx}")),
            (
                "path_with_colon_and_slashes",
                format!("{fm}@{K}:path=/a/b:c"),
            ),
        ];
        for (case, given) in ok {
            let line = argv(&["doctor", "explain", &given]);
            let asked = resolve(&given, &line).unwrap_or_else(|e| panic!("{case}: {e:?}")); // ubs:ignore — a table case's own failure
            assert_eq!(
                (asked.row.id, asked.instance.as_deref()),
                (fm, Some(given.as_str())),
                "{case}"
            );
        }
        let bad = [
            (
                "unknown_fm_part_close",
                format!("fm-concurrency_primitives-journal-pendng@{K}"),
                Some(format!("{fm}@{K}")),
            ),
            ("unknown_fm_part_far", format!("fm-nope-nope@{K}"), None),
            ("scope_is_an_id_not_a_key", format!("{fm}@retail"), None),
            ("unknown_discriminator", format!("{fm}@{K}:nope=1"), None),
        ];
        for (case, given, fixed) in bad {
            let line = argv(&["doctor", "explain", &given, "-t", "retail"]);
            let refused = resolve(&given, &line).unwrap_err();
            assert_eq!(refused.code, ErrorCode::Usage, "{case}");
            let want = match &fixed {
                Some(fixed) => argv(&["doctor", "explain", fixed, "-t", "retail"]),
                None => argv(&["help", "doctor", "explain"]),
            };
            assert_eq!(refused.fix.argv(), Some(want.as_slice()), "{case}");
        }
    }

    #[test]
    fn closest_id_within_two() {
        for (case, given, want) in [
            (
                "one_edit",
                "fm-concurrency_primitives-journal-pendng",
                Some(ids::JOURNAL_PENDING),
            ),
            ("slug_alone", "target-vanishd", Some(ids::TARGET_VANISHED)),
            ("too_far", "fm-nope", None),
        ] {
            let line = argv(&["doctor", "explain", given, "--json"]);
            let refused = resolve(given, &line).unwrap_err();
            let fix = want.map_or_else(
                || argv(&["help", "doctor", "explain"]),
                |id| argv(&["doctor", "explain", id]),
            );
            assert_eq!(refused.fix.argv(), Some(fix.as_slice()), "{case}");
            assert!(
                refused.message.contains(given),
                "{case}: {}",
                refused.message
            );
        }
        let known = resolve(ids::TARGET_VANISHED, &argv(&["doctor", "explain"])).unwrap();
        assert_eq!((known.row.id, known.instance), (ids::TARGET_VANISHED, None));
    }

    #[test]
    fn explain_report_shape() {
        let row = registry::row(ids::TARGET_VANISHED).unwrap();
        let report = ExplainReport {
            failure_mode: ExplainedFailureMode::of(row),
            instances: Vec::new(),
            skipped: Vec::new(),
            incomplete: None,
        };
        let text = serde_json::to_string(&report).unwrap();
        let at = |key: &str| text.find(&format!("\"{key}\":")).unwrap_or(usize::MAX);
        assert!(
            at("failure_mode") < at("instances")
                && at("instances") < at("skipped")
                && at("skipped") < at("incomplete"),
            "{text}"
        );
        let value = serde_json::to_value(&report).unwrap();
        assert_eq!(value["failure_mode"]["fixer"], serde_json::Value::Null);
        assert_eq!(value["failure_mode"]["tier"], "cached");
        assert_eq!(value["failure_mode"]["severity"], "P2");
        assert_eq!(
            value["failure_mode"]["manual"]["instruction"],
            row.manual.unwrap().instruction
        );
        assert_eq!(value["incomplete"], serde_json::Value::Null);
        assert_eq!(
            value.as_object().unwrap().keys().collect::<Vec<_>>(),
            ["failure_mode", "incomplete", "instances", "skipped"],
            "a Value sorts its keys: these four, no more"
        );
    }

    /// A home whose target registry does not decode: one `state-corrupt` instance, about
    /// the machine, which the view itself reports.
    fn corrupt_registry() -> (tempfile::TempDir, DoctorView, String) {
        use crate::doctor::view::tests::{Plant, home, plant, view_at};
        let (dir, paths) = home();
        plant(&paths.registry_file(), &Plant::Bytes(b"{not json".to_vec()));
        let key = format!(
            "{}@machine:path={}",
            ids::STATE_CORRUPT,
            paths.registry_file().display()
        );
        (dir, view_at(paths), key)
    }

    #[test]
    fn filtering_by_key_and_target() {
        let (_dir, view, key) = corrupt_registry();
        let instances = |given: &str, target: Option<&str>| -> Vec<String> {
            let line = argv(&["doctor", "explain", given]);
            let asked = resolve(given, &line).unwrap();
            let report = explain(&view, &asked, target).unwrap();
            report
                .instances
                .iter()
                .map(|f| f.instance.as_str().to_owned())
                .collect()
        };
        let other = format!(
            "{}@machine:path=/elsewhere/registry.json",
            ids::STATE_CORRUPT
        );
        for (case, given, target, want) in [
            (
                "every_instance",
                ids::STATE_CORRUPT,
                None,
                vec![key.clone()],
            ),
            ("by_its_key", key.as_str(), None, vec![key.clone()]),
            ("a_key_that_is_gone", other.as_str(), None, Vec::new()),
            (
                "a_machine_instance_stays_under_t",
                ids::STATE_CORRUPT,
                Some("retail"),
                vec![key.clone()],
            ),
        ] {
            assert_eq!(instances(given, target), want, "{case}");
        }
    }

    #[test]
    fn rows_that_are_not_checked_say_so() {
        let (_dir, view, _) = corrupt_registry();
        let explained = |row: &'static FailureMode| {
            let asked = Asked {
                row,
                instance: None,
            };
            explain(&view, &asked, None)
        };
        // A runtime row has no check: its instances live in a run's report.
        let runtime = explained(registry::row(ids::CHECK_INCOMPLETE).unwrap()).unwrap();
        assert_eq!(
            (
                runtime.instances.len(),
                runtime.skipped.len(),
                runtime.incomplete
            ),
            (0, 0, None)
        );
        // One that needs the network is never run: explain makes no request.
        let online: &'static FailureMode = Box::leak(Box::new(FailureMode {
            online: true,
            ..*registry::row(ids::WIDGET_VERSION_DRIFT).unwrap()
        }));
        let report = explained(online).unwrap();
        assert_eq!(
            report.skipped,
            [Skipped {
                id: ids::WIDGET_VERSION_DRIFT,
                reason: SkipReason::OnlineRequired,
                target: None,
            }]
        );
        assert!(report.instances.is_empty());
        // One whose check panics is a bug, never an empty answer.
        fn panics(
            _: &DoctorView,
            _: &Selection,
            _: &mut Budget,
        ) -> super::super::finding::Detected {
            panic!("a planted panic") // ubs:ignore — the case under test
        }
        let crashing: &'static FailureMode = Box::leak(Box::new(FailureMode {
            detect: Some(panics),
            ..*registry::row(ids::WIDGET_VERSION_DRIFT).unwrap()
        }));
        let refused = explained(crashing).unwrap_err();
        assert_eq!(refused.code, ErrorCode::Internal);
        assert!(
            refused.message.contains(ids::WIDGET_VERSION_DRIFT),
            "{}",
            refused.message
        );
    }

    #[test]
    fn every_row_has_its_manual_or_fixer_explained() {
        for row in FAILURE_MODES {
            let explained = ExplainedFailureMode::of(row);
            assert!(
                explained.fixer.is_some() || explained.manual.is_some(),
                "{}",
                row.id
            );
        }
    }
}
