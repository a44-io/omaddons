//! What a `doctor diagnose` checks (§6.11): its flags turned into the rows to run, their
//! tiers and budgets, and the checks it skips for the network, or the refusal that stops
//! it before anything starts. Pure: it reads only the registry and the flags.

use std::time::Duration;

use super::finding::Severity;
use super::registry::{FailureMode, Tier};
use super::runtime::Budgets;
use super::selection::{Pick, Selection, SkipReason, Skipped};
use crate::cli::{fix_argv, fix_argv_with_value};
use crate::error::{Error, ErrorCode, Result, Suggestion};

/// What the flags that are not a selection ask for.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Asked {
    /// `--severity`: findings less severe are hidden, never the runtime's own.
    pub severity: Option<Severity>,
    /// `--quick`: the cached and quick tiers only.
    pub quick: bool,
    /// `--include-deep`: every tier.
    pub include_deep: bool,
    /// `--online`: checks that need the network run too.
    pub online: bool,
    /// `--budget`: the run's whole time.
    pub budget: Option<Duration>,
}

/// A run's checks, ready for the runtime.
#[derive(Debug, Clone)]
pub struct DiagnosePlan {
    /// The rows to run, in registry order.
    pub rows: Vec<FailureMode>,
    /// The tiers they are of.
    pub tiers: Vec<Tier>,
    pub budgets: Budgets,
    /// Selected checks that need the network, without `--online`; sorted by id.
    pub online_skipped: Vec<Skipped>,
    pub max_severity: Option<Severity>,
}

/// Every tier a check can be of; `runtime` rows are no check.
const CHECK_TIERS: [Tier; 4] = [Tier::Cached, Tier::Quick, Tier::Default, Tier::Deep];

/// The checks `sel` and `asked` select from `registry`, with `argv` (the command line) for
/// the fixes of the refusals: `usage` (a runtime row named, or nothing left to check),
/// `online_required` (only checks that need the network, without `--online`), and
/// `doctor_refused` (the selection's declared cost is over `--budget`).
pub fn plan_diagnose(
    registry: &[FailureMode],
    sel: &Selection,
    asked: &Asked,
    argv: &[String],
) -> Result<DiagnosePlan> {
    // The runtime's rows are reported by a run, never checked by one.
    for (flag, picks) in [("--only", &sel.only), ("--skip", &sel.skip)] {
        for pick in picks.iter() {
            if let Pick::Mode(id) = pick
                && registry
                    .iter()
                    .any(|row| row.id == *id && row.tier == Tier::Runtime)
            {
                return Err(usage(
                    format!("{id} is reported by the run, not a check: it cannot be in {flag}"),
                    without_item(argv, flag, id),
                ));
            }
        }
    }
    let candidates: Vec<&FailureMode> = registry
        .iter()
        .filter(|row| row.tier != Tier::Runtime && sel.includes(row))
        .collect();
    if candidates.is_empty() {
        return Err(usage(
            "nothing to check: --skip leaves no check".to_owned(),
            without(argv, "--skip", true),
        ));
    }

    // `--only` alone runs exactly what it names, whatever the tier.
    let (tiers, tier_flag) = if asked.quick {
        (vec![Tier::Cached, Tier::Quick], Some("--quick"))
    } else if asked.include_deep || !sel.only.is_empty() {
        (CHECK_TIERS.to_vec(), None)
    } else {
        (vec![Tier::Cached, Tier::Quick, Tier::Default], None)
    };
    let in_tier: Vec<&FailureMode> = candidates
        .iter()
        .copied()
        .filter(|row| tiers.contains(&row.tier))
        .collect();
    if in_tier.is_empty() {
        let excluded = describe(&candidates, |row| format!("tier {}", tier_name(row.tier)));
        return Err(match tier_flag {
            Some(flag) => usage(
                format!("{flag} excludes {excluded}"),
                without(argv, flag, false),
            ),
            None => usage(
                format!(
                    "only deep checks are selected ({excluded}), and deep checks run with --include-deep"
                ),
                with_flag(argv, "--include-deep"),
            ),
        });
    }

    let kept: Vec<&FailureMode> = in_tier
        .iter()
        .copied()
        .filter(|row| asked.severity.is_none_or(|max| row.severity <= max))
        .collect();
    if let (true, Some(max)) = (kept.is_empty(), asked.severity) {
        let excluded = describe(&in_tier, |row| format!("{:?}", row.severity));
        return Err(usage(
            format!("--severity {max:?} excludes {excluded}"),
            without(argv, "--severity", true),
        ));
    }

    let (online, offline): (Vec<&FailureMode>, Vec<&FailureMode>) =
        kept.iter().partition(|row| row.online && !asked.online);
    if offline.is_empty() {
        let fms: Vec<&str> = online.iter().map(|row| row.id).collect();
        return Err(Error::new(
            ErrorCode::OnlineRequired,
            format!("{} need the network: add --online", fms.join(", ")),
            Suggestion::command(with_flag(argv, "--online")),
        )
        .details(serde_json::json!({ "fms": fms })));
    }
    let mut online_skipped: Vec<Skipped> = online
        .iter()
        .map(|row| Skipped {
            id: row.id,
            reason: SkipReason::OnlineRequired,
            target: None,
        })
        .collect();
    online_skipped.sort_by(|a, b| a.id.cmp(b.id));

    let budgets = match asked.budget {
        None => Budgets::TierDefaults,
        Some(budget) => {
            let estimated: u64 = offline.iter().map(|row| u64::from(row.cost_ms)).sum();
            let budget_ms = u64::try_from(budget.as_millis()).unwrap_or(u64::MAX);
            if estimated > budget_ms {
                let enough = estimated.div_ceil(100) * 100;
                return Err(Error::new(
                    ErrorCode::DoctorRefused,
                    format!(
                        "the selected checks are estimated to take {estimated} ms, more than --budget {budget_ms}ms"
                    ),
                    Suggestion::command(with_value(argv, "--budget", &format!("{enough}ms"))),
                )
                .alternative(Suggestion::command(without(argv, "--budget", true)))
                .details(serde_json::json!({
                    "reason": "over_budget",
                    "budget_ms": budget_ms,
                    "estimated_ms": estimated,
                })));
            }
            Budgets::Run(budget)
        }
    };
    Ok(DiagnosePlan {
        rows: offline.into_iter().copied().collect(),
        tiers,
        budgets,
        online_skipped,
        max_severity: asked.severity,
    })
}

fn usage(message: String, fix: Vec<String>) -> Error {
    Error::new(ErrorCode::Usage, message, Suggestion::command(fix)).user_verb()
}

fn tier_name(tier: Tier) -> &'static str {
    match tier {
        Tier::Cached => "cached",
        Tier::Quick => "quick",
        Tier::Default => "default",
        Tier::Deep => "deep",
        Tier::Runtime => "runtime",
    }
}

/// `fm-x (what)`, for up to three rows, then how many more.
fn describe(rows: &[&FailureMode], what: impl Fn(&FailureMode) -> String) -> String {
    let mut named: Vec<String> = rows
        .iter()
        .take(3)
        .map(|row| format!("{} ({})", row.id, what(row)))
        .collect();
    if rows.len() > 3 {
        named.push(format!("{} more", rows.len() - 3));
    }
    named.join(", ")
}

/// Whether `arg` is `flag` given its value inline (`--budget=2s`).
fn inline(arg: &str, flag: &str) -> bool {
    arg.strip_prefix(flag)
        .is_some_and(|rest| rest.starts_with('='))
}

/// `argv` without every `flag` (and its value, when it takes one).
fn without(argv: &[String], flag: &str, takes_value: bool) -> Vec<String> {
    let mut out = Vec::new();
    let mut args = argv.iter().enumerate();
    while let Some((at, arg)) = args.next() {
        if at > 0 && arg == flag {
            if takes_value {
                args.next();
            }
            continue;
        }
        if at > 0 && takes_value && inline(arg, flag) {
            continue;
        }
        out.push(arg.clone());
    }
    fix_argv(&out)
}

/// `argv` with a bare `flag` added, unless it has one.
fn with_flag(argv: &[String], flag: &str) -> Vec<String> {
    let mut out = argv.to_vec();
    if !out.iter().skip(1).any(|arg| arg == flag) {
        out.push(flag.to_owned());
    }
    fix_argv(&out)
}

/// `argv` with `value` in place of the value `flag` had, else `flag value` added.
fn with_value(argv: &[String], flag: &str, value: &str) -> Vec<String> {
    fix_argv_with_value(argv, flag, value).unwrap_or_else(|| {
        let mut out = fix_argv(argv);
        out.extend([flag.to_owned(), value.to_owned()]);
        out
    })
}

/// `argv` with `item` taken out of each comma list `flag` was given; a list left empty
/// takes its flag with it.
fn without_item(argv: &[String], flag: &str, item: &str) -> Vec<String> {
    let kept = |value: &str| -> String {
        value
            .split(',')
            .filter(|i| *i != item)
            .collect::<Vec<_>>()
            .join(",")
    };
    let mut out: Vec<String> = Vec::new();
    let mut args = argv.iter().enumerate();
    while let Some((at, arg)) = args.next() {
        if at > 0 && arg == flag {
            let rest = args
                .next()
                .map(|(_, value)| kept(value))
                .unwrap_or_default();
            if !rest.is_empty() {
                out.extend([flag.to_owned(), rest]);
            }
            continue;
        }
        if at > 0 && inline(arg, flag) {
            let rest = kept(&arg[flag.len() + 1..]);
            if !rest.is_empty() {
                out.push(format!("{flag}={rest}"));
            }
            continue;
        }
        out.push(arg.clone());
    }
    fix_argv(&out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doctor::finding::Subsystem;
    use crate::doctor::registry::ids;

    const C1: &str = "fm-state_files-plan-cached-one";
    const C2: &str = "fm-state_files-plan-cached-two";
    const Q: &str = "fm-state_files-plan-quick";
    const D: &str = "fm-state_files-plan-default";
    const DEEP: &str = "fm-configs-plan-deep";
    const ONLINE: &str = "fm-state_files-plan-online";

    fn row(id: &'static str, tier: Tier, severity: Severity, cost_ms: u32) -> FailureMode {
        let mut row = *crate::doctor::registry::row(ids::TARGET_VANISHED).unwrap();
        row.id = id;
        row.tier = tier;
        row.severity = severity;
        row.cost_ms = cost_ms;
        row.subsystem = if id == DEEP {
            Subsystem::Configs
        } else {
            Subsystem::StateFiles
        };
        row
    }

    /// Two cached rows, a quick one, a default one (P3), a deep one, an online one, and
    /// the runtime's two.
    fn registry() -> Vec<FailureMode> {
        let mut online = row(ONLINE, Tier::Default, Severity::P2, 50);
        online.online = true;
        let runtime = |id| *crate::doctor::registry::row(id).unwrap();
        vec![
            row(C1, Tier::Cached, Severity::P1, 1),
            row(C2, Tier::Cached, Severity::P2, 1),
            row(Q, Tier::Quick, Severity::P2, 4),
            row(D, Tier::Default, Severity::P3, 20),
            row(DEEP, Tier::Deep, Severity::P2, 500),
            online,
            runtime(ids::CHECK_INCOMPLETE),
            runtime(ids::DETECTOR_CRASHED),
        ]
    }

    fn argv(words: &[&str]) -> Vec<String> {
        std::iter::once("omaddons")
            .chain(words.iter().copied())
            .map(str::to_owned)
            .collect()
    }

    fn sel(only: &[&'static str], skip: &[&'static str]) -> Selection {
        Selection {
            target: None,
            only: only.iter().copied().map(Pick::Mode).collect(),
            skip: skip.iter().copied().map(Pick::Mode).collect(),
        }
    }

    fn ids_of(plan: &DiagnosePlan) -> Vec<&'static str> {
        plan.rows.iter().map(|row| row.id).collect()
    }

    #[test]
    fn plan_diagnose_cases() {
        let asked = |edit: fn(&mut Asked)| {
            let mut asked = Asked::default();
            edit(&mut asked);
            asked
        };
        let subsystem = Selection {
            only: vec![Pick::Subsystem(Subsystem::Configs)],
            ..Selection::all()
        };
        let cases: [(&str, Selection, Asked, &[&str]); 9] = [
            (
                "default_runs_every_tier_but_deep",
                Selection::all(),
                Asked::default(),
                &[C1, C2, Q, D],
            ),
            (
                "quick_runs_cached_and_quick",
                Selection::all(),
                asked(|a| a.quick = true),
                &[C1, C2, Q],
            ),
            (
                "include_deep_runs_all",
                Selection::all(),
                asked(|a| a.include_deep = true),
                &[C1, C2, Q, D, DEEP],
            ),
            (
                "only_a_deep_row_runs_it",
                sel(&[DEEP], &[]),
                Asked::default(),
                &[DEEP],
            ),
            (
                "skip_removes_a_row",
                sel(&[], &[C1]),
                Asked::default(),
                &[C2, Q, D],
            ),
            (
                "only_a_subsystem_selects_its_rows",
                subsystem,
                Asked::default(),
                &[DEEP],
            ),
            (
                "severity_p2_drops_p3_rows",
                Selection::all(),
                asked(|a| a.severity = Some(Severity::P2)),
                &[C1, C2, Q],
            ),
            (
                "online_runs_with_online",
                sel(&[ONLINE], &[]),
                asked(|a| a.online = true),
                &[ONLINE],
            ),
            (
                "mixed_online_runs_the_offline_ones",
                Selection::all(),
                Asked::default(),
                &[C1, C2, Q, D],
            ),
        ];
        for (case, sel, asked, expected) in cases {
            let plan = plan_diagnose(&registry(), &sel, &asked, &argv(&["doctor"]));
            assert!(plan.is_ok(), "{case}: {sel:?} {asked:?}: {:?}", plan.err());
            let plan = plan.unwrap();
            assert_eq!(ids_of(&plan), expected, "{case}: {sel:?} {asked:?}");
            assert_eq!(plan.budgets, Budgets::TierDefaults, "{case}");
        }
    }

    #[test]
    fn mixed_online_skips_with_online_required() {
        let plan = plan_diagnose(
            &registry(),
            &Selection::all(),
            &Asked::default(),
            &argv(&["doctor"]),
        )
        .unwrap();
        assert_eq!(
            plan.online_skipped,
            [Skipped {
                id: ONLINE,
                reason: SkipReason::OnlineRequired,
                target: None
            }]
        );
    }

    /// A refusal case: its name, what was asked on which command line, then the code, a
    /// phrase of the message and the fix.
    type Refusal<'a> = (
        &'a str,
        Selection,
        Asked,
        Vec<&'a str>,
        ErrorCode,
        &'a str,
        &'a [&'a str],
    );

    /// Each refusal: its code, a phrase of its message, and its fix.
    #[test]
    fn refusals() {
        let only_runtime = format!("{},{C1}", ids::CHECK_INCOMPLETE);
        let skip_runtime = format!("--skip={}", ids::DETECTOR_CRASHED);
        let budget = Asked {
            budget: Some(Duration::from_millis(5)),
            ..Asked::default()
        };
        let cases: [Refusal; 5] = [
            (
                "quick_with_only_a_default_row_is_usage_naming_quick",
                sel(&[D], &[]),
                Asked {
                    quick: true,
                    ..Asked::default()
                },
                vec!["doctor", "--only", D, "--quick"],
                ErrorCode::Usage,
                "--quick excludes",
                &["doctor", "--only", D],
            ),
            (
                "only_a_runtime_row_is_usage",
                sel(&[ids::CHECK_INCOMPLETE, C1], &[]),
                Asked::default(),
                vec!["doctor", "--only", &only_runtime],
                ErrorCode::Usage,
                "is reported by the run",
                &["doctor", "--only", C1],
            ),
            (
                "skip_a_runtime_row_is_usage",
                sel(&[], &[ids::DETECTOR_CRASHED]),
                Asked::default(),
                vec!["doctor", &skip_runtime],
                ErrorCode::Usage,
                "is reported by the run",
                &["doctor"],
            ),
            (
                "online_only_without_online_is_online_required",
                sel(&[ONLINE], &[]),
                Asked::default(),
                vec!["doctor", "--only", ONLINE],
                ErrorCode::OnlineRequired,
                "need the network",
                &["doctor", "--only", ONLINE, "--online"],
            ),
            (
                "over_budget_is_doctor_refused",
                Selection::all(),
                budget,
                vec!["doctor", "--budget", "5ms"],
                ErrorCode::DoctorRefused,
                "estimated to take 26 ms",
                &["doctor", "--budget", "100ms"],
            ),
        ];
        for (case, sel, asked, line, code, phrase, fix) in cases {
            let refused = plan_diagnose(&registry(), &sel, &asked, &argv(&line)).err();
            assert!(refused.is_some(), "{case}: no refusal");
            let refused = refused.unwrap();
            assert_eq!(refused.code, code, "{case}: {}", refused.message);
            assert!(
                refused.message.contains(phrase),
                "{case}: {}",
                refused.message
            );
            assert_eq!(refused.fix.argv(), Some(argv(fix).as_slice()), "{case}");
        }

        let refused = plan_diagnose(
            &registry(),
            &sel(&[ONLINE], &[]),
            &Asked::default(),
            &argv(&["doctor"]),
        )
        .unwrap_err();
        assert_eq!(refused.details, serde_json::json!({ "fms": [ONLINE] }));

        let refused = plan_diagnose(
            &registry(),
            &Selection::all(),
            &budget,
            &argv(&["doctor", "--budget", "5ms"]),
        )
        .unwrap_err();
        assert_eq!(
            refused.details,
            serde_json::json!({"reason": "over_budget", "budget_ms": 5, "estimated_ms": 26})
        );
        assert_eq!(
            refused.alternatives.first().and_then(|a| a.argv()),
            Some(argv(&["doctor"]).as_slice()),
            "the alternative runs without --budget"
        );
    }

    #[test]
    fn a_budget_that_fits_gives_shares() {
        let budget = Duration::from_millis(500);
        let plan = plan_diagnose(
            &registry(),
            &Selection::all(),
            &Asked {
                budget: Some(budget),
                ..Asked::default()
            },
            &argv(&["doctor", "--budget", "500ms"]),
        )
        .unwrap();
        assert_eq!(plan.budgets, Budgets::Run(budget));
    }
}
