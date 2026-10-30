//! The doctor's report (M4-D34, A.4): what one diagnose found, carried at the top level
//! of the doctor envelope and recorded as its run's `report.json`. Pure.

use std::collections::BTreeSet;

use jiff::Timestamp;
use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde::Serialize;

use super::finding::{Finding, Severity};
use super::registry::ids;
use super::runtime::{Diagnosis, DoctorRunId};
use super::selection::{SkipReason, Skipped};
use crate::error::{CommandHint, ErrorBody, Warning};
use crate::output::RunKind;

/// The version of the doctor contract the report follows (M4-D3).
pub const DOCTOR_CONTRACT_VERSION: &str = "1.0";

/// A diagnose's report, by the view it was asked for.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(tag = "view")]
pub enum DiagnoseReport {
    /// Every finding, with its evidence and remediation.
    #[serde(rename = "full")]
    Full(FullReport),
}

/// The full view of a diagnose.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct FullReport {
    pub doctor_contract_version: &'static str,
    pub run_id: DoctorRunId,
    /// The run's directory, absolute.
    pub run_dir: String,
    #[schemars(with = "String")]
    pub started_at: Timestamp,
    #[schemars(with = "String")]
    pub finished_at: Timestamp,
    pub state: RunState,
    pub summary: Summary,
    /// Ordered by severity, failure mode, client and instance.
    pub findings: Vec<Finding>,
    /// The checks the run did not make, and why: never a pass.
    pub skipped: Vec<Skipped>,
    /// What changed since the run `--since` named. Omitted without `--since`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub since: Option<Since>,
    /// The `doctor explain` command of each failure mode found, as `commands[]` holds
    /// them.
    pub next_steps: Vec<String>,
}

impl FullReport {
    /// The report of `diagnosis`, the run `run_id` in `run_dir`.
    pub fn new(
        run_id: DoctorRunId,
        run_dir: String,
        diagnosis: Diagnosis,
        since: Option<Since>,
    ) -> Self {
        let summary = summary(
            &diagnosis.findings,
            &diagnosis.skipped,
            diagnosis.duration_ms,
        );
        let next_steps = explain_argvs(&diagnosis.findings)
            .iter()
            .map(shell_words::join)
            .collect();
        Self {
            doctor_contract_version: DOCTOR_CONTRACT_VERSION,
            run_id,
            run_dir,
            started_at: diagnosis.started_at,
            finished_at: diagnosis.finished_at,
            state: RunState::of(&diagnosis.findings),
            summary,
            findings: diagnosis.findings,
            skipped: diagnosis.skipped,
            since,
            next_steps,
        }
    }
}

/// Where a run ended (the doctor contract's names, A.4); `doctor fix` adds its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RunState {
    /// Every selected check ran and found nothing.
    DoneOk,
    /// Something was found, a check that could not finish included.
    DoneFindings,
}

impl RunState {
    /// `DONE_OK` only with no finding at all.
    pub fn of(findings: &[Finding]) -> Self {
        if findings.is_empty() {
            Self::DoneOk
        } else {
            Self::DoneFindings
        }
    }
}

/// A report's counts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Summary {
    pub total_findings: u32,
    pub by_severity: BySeverity,
    /// Findings `doctor fix` repairs without `--force`.
    pub auto_fixable: u32,
    /// Findings whose fix runs only with `--force`.
    pub needs_force: u32,
    /// Findings no fixer repairs: the total less the two above.
    pub manual: u32,
    /// Checks skipped because they need `--online`.
    pub online_required: u32,
    /// Checks the run did not make.
    pub skipped: u32,
    /// Checks a limit cut short.
    pub incomplete: u32,
    pub duration_ms: u64,
}

/// Findings per severity, all four always present.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, JsonSchema)]
pub struct BySeverity {
    #[serde(rename = "P0")]
    pub p0: u32,
    #[serde(rename = "P1")]
    pub p1: u32,
    #[serde(rename = "P2")]
    pub p2: u32,
    #[serde(rename = "P3")]
    pub p3: u32,
}

/// What changed since an earlier run, by instance key (M4-D30), never by label.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Since {
    pub run: DoctorRunId,
    /// Instances found now and not then, sorted.
    pub new_instances: Vec<String>,
    /// Instances found then and not now, sorted.
    pub resolved_instances: Vec<String>,
    /// Instances found both times.
    pub unchanged: u32,
}

/// The counts of a report of `findings` and `skipped`.
pub fn summary(findings: &[Finding], skipped: &[Skipped], duration_ms: u64) -> Summary {
    let count = |n: usize| u32::try_from(n).unwrap_or(u32::MAX);
    let mut by_severity = BySeverity::default();
    for finding in findings {
        let slot = match finding.severity {
            Severity::P0 => &mut by_severity.p0,
            Severity::P1 => &mut by_severity.p1,
            Severity::P2 => &mut by_severity.p2,
            Severity::P3 => &mut by_severity.p3,
        };
        *slot += 1;
    }
    let total_findings = count(findings.len());
    let auto_fixable = count(
        findings
            .iter()
            .filter(|f| f.remediation.auto_fixable)
            .count(),
    );
    let needs_force = count(
        findings
            .iter()
            .filter(|f| f.remediation.needs_force)
            .count(),
    );
    Summary {
        total_findings,
        by_severity,
        auto_fixable,
        needs_force,
        manual: total_findings.saturating_sub(auto_fixable + needs_force),
        online_required: count(
            skipped
                .iter()
                .filter(|s| s.reason == SkipReason::OnlineRequired)
                .count(),
        ),
        skipped: count(skipped.len()),
        incomplete: count(
            findings
                .iter()
                .filter(|f| f.id == ids::CHECK_INCOMPLETE)
                .count(),
        ),
        duration_ms,
    }
}

/// `--since`: `new` against the instance keys of the run `run` reported, `old`.
pub fn since(run: DoctorRunId, old: &[String], new: &[Finding]) -> Since {
    let old: BTreeSet<&str> = old.iter().map(String::as_str).collect();
    let now: BTreeSet<&str> = new.iter().map(|f| f.instance.as_str()).collect();
    Since {
        run,
        new_instances: now.difference(&old).map(|key| (*key).to_owned()).collect(),
        resolved_instances: old.difference(&now).map(|key| (*key).to_owned()).collect(),
        unchanged: u32::try_from(now.intersection(&old).count()).unwrap_or(u32::MAX),
    }
}

/// One `doctor explain` argv per failure mode in `findings`, in their order, with
/// `-t <client>` when every instance of it is on that one client.
pub fn explain_argvs(findings: &[Finding]) -> Vec<Vec<String>> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for finding in findings {
        if !seen.insert(finding.id) {
            continue;
        }
        let targets: BTreeSet<Option<&str>> = findings
            .iter()
            .filter(|f| f.id == finding.id)
            .map(|f| f.target.as_ref().map(|t| t.id.as_str()))
            .collect();
        let mut argv: Vec<String> = ["omaddons", "doctor", "explain", finding.id]
            .map(str::to_owned)
            .into();
        if let [Some(target)] = targets.into_iter().collect::<Vec<_>>()[..] {
            argv.extend(["-t".to_owned(), target.to_owned()]);
        }
        out.push(argv);
    }
    out
}

/// A run's `report.json`: the doctor envelope's record form (its `kind`, no `meta`) with
/// a diagnose report. Only its schema is used: the runtime writes the bytes the command
/// built.
#[derive(Serialize, JsonSchema)]
struct ReportRecord {
    ok: bool,
    schema_version: String,
    tool_version: String,
    command: String,
    kind: RunKind,
    exit_code: u8,
    #[serde(flatten)]
    report: DiagnoseReport,
    /// Present when the run failed after its report was made.
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<ErrorBody>,
    warnings: Vec<Warning>,
    commands: Vec<CommandHint>,
}

/// The schema of a run's `report.json` (`store::KINDS`' `doctor_report`).
pub fn record_schema(g: &mut SchemaGenerator) -> Schema {
    g.root_schema_for::<ReportRecord>()
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::doctor::finding::{Confidence, Evidence, Observation, Subject};
    use crate::doctor::registry;
    use crate::doctor::selection::{SkipReason, Skipped};
    use crate::model::{TargetKey, TxId};
    use crate::output::{DoctorEnvelope, Meta};

    const K: &str = "01M5ZQ7Y3KJ8V9W2X4C6B0N1RT";
    const K2: &str = "01M5ZQ7Y3KJ8V9W2X4C6B0N1RV";
    const TX_A: &str = "01M7A1B2C3D4E5F6G7H8J9K0MN";
    const TX_B: &str = "01M7A1B2C3D4E5F6G7H8J9K0MP";
    const RUN: &str = "2026-10-30T18-03-40Z__01M8S7XQKZ0000000000000ABC";

    fn on(id: &str, key: &str) -> Subject {
        Subject::target(id, key.parse::<TargetKey>().unwrap())
    }

    /// The finding of the registry row `fm` about `subject`.
    fn finding(fm: &str, subject: Subject) -> Finding {
        registry::row(fm).unwrap().finding(Observation {
            subject,
            title: "t".to_owned(),
            confidence: Confidence::High,
            addon: None,
            folder: None,
            evidence: Evidence::default(),
        })
    }

    fn journal(tx: &str) -> Finding {
        finding(
            registry::ids::JOURNAL_PENDING,
            on("retail", K).tx(tx.parse::<TxId>().unwrap()),
        )
    }

    fn skill(path: &str) -> Finding {
        finding(
            registry::ids::STATE_CORRUPT,
            Subject::machine().path(Path::new(path)),
        )
    }

    fn keys(findings: &[Finding]) -> Vec<String> {
        findings
            .iter()
            .map(|f| f.instance.as_str().to_owned())
            .collect()
    }

    fn run() -> DoctorRunId {
        DoctorRunId::parse(RUN).unwrap()
    }

    #[test]
    fn since_cases() {
        let (a, b) = (journal(TX_A), journal(TX_B));
        let (x, y) = (skill("/x/a/SKILL.md"), skill("/x/b/SKILL.md"));
        let cases = [
            (
                "two journals, one resolved",
                keys(&[a.clone(), b.clone()]),
                vec![b.clone()],
                0,
                1,
                1,
            ),
            (
                "two skill roots, one resolved",
                keys(&[x.clone(), y.clone()]),
                vec![y.clone()],
                0,
                1,
                1,
            ),
            (
                "a new instance",
                keys(std::slice::from_ref(&b)),
                vec![a.clone(), b.clone()],
                1,
                0,
                1,
            ),
            (
                "an empty earlier report",
                Vec::new(),
                vec![a.clone(), b.clone()],
                2,
                0,
                0,
            ),
        ];
        for (case, old, new, added, resolved, unchanged) in cases {
            let got = since(run(), &old, &new);
            assert_eq!(
                (
                    got.new_instances.len(),
                    got.resolved_instances.len(),
                    got.unchanged
                ),
                (added, resolved, unchanged),
                "{case}: {got:?}"
            );
            let mut sorted = got.new_instances.clone();
            sorted.sort();
            assert_eq!(got.new_instances, sorted, "{case}: sorted");
        }
        let got = since(run(), &keys(&[a.clone(), b.clone()]), &[b]);
        assert_eq!(
            got.resolved_instances,
            keys(&[a]),
            "by instance key, never by label"
        );
    }

    #[test]
    fn summary_counts() {
        let findings = [
            journal(TX_A),
            finding(
                registry::ids::CHECK_INCOMPLETE,
                Subject::machine().detector(registry::ids::STATE_CORRUPT),
            ),
            finding(registry::ids::TARGET_VANISHED, on("era", K2)),
        ];
        let skipped = [Skipped {
            id: registry::ids::STATE_CORRUPT,
            reason: SkipReason::ConfigUnavailable,
            target: None,
        }];
        let got = summary(&findings, &skipped, 7);
        assert_eq!(
            got,
            Summary {
                total_findings: 3,
                by_severity: BySeverity {
                    p0: 1,
                    p1: 0,
                    p2: 1,
                    p3: 1
                },
                auto_fixable: 0,
                needs_force: 0,
                manual: 3,
                online_required: 0,
                skipped: 1,
                incomplete: 1,
                duration_ms: 7,
            }
        );
        let empty = serde_json::to_value(summary(&[], &[], 0)).unwrap();
        assert_eq!(
            empty["by_severity"],
            serde_json::json!({"P0": 0, "P1": 0, "P2": 0, "P3": 0}),
            "all four, always"
        );
    }

    #[test]
    fn done_ok_needs_no_finding_at_all() {
        assert_eq!(RunState::of(&[]), RunState::DoneOk);
        let incomplete = finding(
            registry::ids::CHECK_INCOMPLETE,
            Subject::machine().detector(registry::ids::STATE_CORRUPT),
        );
        assert_eq!(
            RunState::of(&[incomplete]),
            RunState::DoneFindings,
            "one P3 is enough"
        );
        assert_eq!(
            serde_json::to_value([RunState::DoneOk, RunState::DoneFindings]).unwrap(),
            serde_json::json!(["DONE_OK", "DONE_FINDINGS"])
        );
    }

    #[test]
    fn explain_argvs_name_one_client_only_when_all_are_on_it() {
        let argv =
            |words: &[&str]| -> Vec<String> { words.iter().map(|w| (*w).to_owned()).collect() };
        let one_client = [journal(TX_A), journal(TX_B)];
        assert_eq!(
            explain_argvs(&one_client),
            [argv(&[
                "omaddons",
                "doctor",
                "explain",
                registry::ids::JOURNAL_PENDING,
                "-t",
                "retail"
            ])]
        );
        let two_clients = [
            finding(registry::ids::TARGET_VANISHED, on("retail", K)),
            finding(registry::ids::TARGET_VANISHED, on("era", K2)),
            skill("/x/a/SKILL.md"),
        ];
        assert_eq!(
            explain_argvs(&two_clients),
            [
                argv(&[
                    "omaddons",
                    "doctor",
                    "explain",
                    registry::ids::TARGET_VANISHED
                ]),
                argv(&[
                    "omaddons",
                    "doctor",
                    "explain",
                    registry::ids::STATE_CORRUPT
                ]),
            ],
            "one per failure mode, in findings order; a machine finding names no client"
        );
    }

    fn report(findings: Vec<Finding>) -> FullReport {
        FullReport::new(
            run(),
            "/fixture/state/omaddons/doctor/runs/x".to_owned(),
            Diagnosis {
                findings,
                skipped: Vec::new(),
                incomplete: 0,
                checks_run: 1,
                started_at: "2026-10-30T18:03:40Z".parse().unwrap(),
                finished_at: "2026-10-30T18:03:41Z".parse().unwrap(),
                duration_ms: 3,
            },
            None,
        )
    }

    #[test]
    fn next_steps_mirror_the_explain_argvs() {
        let full = report(vec![journal(TX_A)]);
        assert_eq!(
            full.next_steps,
            [format!(
                "omaddons doctor explain {} -t retail",
                registry::ids::JOURNAL_PENDING
            )]
        );
        assert_eq!(full.state, RunState::DoneFindings);
    }

    /// A document's top-level keys in order (a `Value` sorts them).
    fn ordered_keys(text: &str) -> Vec<String> {
        use serde::de::{Deserializer as _, IgnoredAny, MapAccess, Visitor};
        struct Keys;
        impl<'de> Visitor<'de> for Keys {
            type Value = Vec<String>;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a JSON object")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Vec<String>, A::Error> {
                let mut keys = Vec::new();
                while let Some(key) = map.next_key::<String>()? {
                    map.next_value::<IgnoredAny>()?;
                    keys.push(key);
                }
                Ok(keys)
            }
        }
        serde_json::Deserializer::from_str(text)
            .deserialize_map(Keys)
            .unwrap()
    }

    #[test]
    fn report_key_order_and_record_form() {
        let diagnose = DiagnoseReport::Full(report(Vec::new()));
        let envelope = DoctorEnvelope::report(
            "doctor diagnose",
            &diagnose,
            crate::doctor::exit::Exit::Healthy,
            None,
            &[],
            &[],
            Meta {
                elapsed_ms: 0,
                offline: false,
            },
        )
        .record(RunKind::Diagnose);
        let text = serde_json::to_string(&envelope).unwrap();
        assert_eq!(
            ordered_keys(&text),
            [
                "ok",
                "schema_version",
                "tool_version",
                "command",
                "kind",
                "exit_code",
                "view",
                "doctor_contract_version",
                "run_id",
                "run_dir",
                "started_at",
                "finished_at",
                "state",
                "summary",
                "findings",
                "skipped",
                "next_steps",
                "warnings",
                "commands",
            ],
            "{text}"
        );
        assert!(
            text.contains(r#""view":"full""#) && text.contains(r#""state":"DONE_OK""#),
            "{text}"
        );
    }

    #[test]
    fn the_record_schema_describes_report_json() {
        let mut generator = schemars::generate::SchemaSettings::draft2020_12()
            .for_serialize()
            .into_generator();
        let schema = record_schema(&mut generator).to_value();
        let text = schema.to_string();
        for key in [
            "\"kind\"",
            "\"view\"",
            "\"run_id\"",
            "\"next_steps\"",
            "\"DONE_FINDINGS\"",
        ] {
            assert!(text.contains(key), "{key} in {text}");
        }
    }
}
