//! The declared-but-unobserved `details` report (M4-D55 rule 6: every declared key appears
//! in at least one test). It counts only what `support::check` sees, since a key observed
//! in another suite is never validated against its spec. H1 makes it empty and an
//! assertion.

use std::collections::{BTreeMap, BTreeSet};

use omaddons::error::ErrorCode;
use serde_json::{Value, json};

use super::validate::observed_details;

#[test]
#[ignore = "a report: run with --ignored --nocapture; H1 makes it empty and an assertion"]
fn declared_details_keys_no_contract_test_observes() {
    // The contract tests run on parallel threads with no hook after them all, so the
    // report runs the suite again as a child that records what `check` observes.
    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("details-observed-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("observed.jsonl");
    let _ = std::fs::remove_file(&file);
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--skip",
            "declared_details_keys_no_contract_test_observes",
            "--quiet",
        ])
        .env("CONTRACT_DETAILS_OBSERVED", &file)
        .status()
        .unwrap();
    let observed: BTreeSet<(String, String)> = std::fs::read_to_string(&file)
        .unwrap_or_default()
        .lines()
        .map(|line| {
            let pair: Value = serde_json::from_str(line).unwrap();
            let field = |key: &str| pair[key].as_str().unwrap().to_owned();
            (field("code"), field("path"))
        })
        .collect();

    let mut shared: BTreeSet<String> = BTreeSet::new();
    let mut unobserved: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    let (mut declared, mut seen) = (0usize, 0usize);
    for code in ErrorCode::ALL {
        let own = code.detail_spec().declared_paths();
        for path in code.declared_detail_paths() {
            if !own.contains(&path) {
                shared.insert(path);
            }
        }
        for path in own {
            declared += 1;
            if observed.contains(&(code.wire().to_owned(), path.clone())) {
                seen += 1;
            } else {
                unobserved.entry(code.wire()).or_default().push(path);
            }
        }
    }
    let mut shared_unobserved = Vec::new();
    for path in &shared {
        declared += 1;
        if observed
            .iter()
            .any(|(_, observed_path)| observed_path == path)
        {
            seen += 1;
        } else {
            shared_unobserved.push(path.clone());
        }
    }

    println!(
        "declared {declared}, observed {seen}, unobserved {}",
        declared - seen
    );
    for (code, paths) in &unobserved {
        println!("  {code}: {}", paths.join(", "));
    }
    println!("  * (shared): {}", shared_unobserved.join(", "));
    println!("child contract run: {status}");
}

#[test]
fn observed_details_paths() {
    type Pairs = BTreeSet<(String, String)>;
    let set = |pairs: &[(&str, &str)]| -> Pairs {
        pairs
            .iter()
            .map(|(code, path)| ((*code).to_owned(), (*path).to_owned()))
            .collect()
    };
    let failure =
        |details: Value| json!({"ok": false, "error": {"code": "plan_stale", "details": details}});
    let cases: Vec<(&str, Value, Pairs)> = vec![
        (
            "flat_keys",
            failure(json!({"cause": "x", "changed": ["selection"]})),
            set(&[("plan_stale", "cause"), ("plan_stale", "changed")]),
        ),
        (
            "nested_object",
            failure(json!({"recovery_error": {"code": "io", "message": "m"}})),
            set(&[
                ("plan_stale", "recovery_error"),
                ("plan_stale", "recovery_error.code"),
                ("plan_stale", "recovery_error.message"),
            ]),
        ),
        (
            "array_of_objects",
            failure(json!({"tried": [{"asset": "a", "reason": "r"}, {"asset": "b"}]})),
            set(&[
                ("plan_stale", "tried"),
                ("plan_stale", "tried[].asset"),
                ("plan_stale", "tried[].reason"),
            ]),
        ),
        (
            "array_of_scalars",
            failure(json!({"required_by": ["A", "B"]})),
            set(&[("plan_stale", "required_by")]),
        ),
        (
            "embedded_target_result_error",
            json!({"ok": true, "data": {"targets": [{"error": {
                "code": "game_running", "details": {"reason": "loaded_toc"},
            }}]}}),
            set(&[("game_running", "reason")]),
        ),
        (
            "item_error_kept_inside_details",
            failure(
                json!({"items": [{"request": "x", "target": "retail", "error": {
                    "code": "rate_limited", "details": {"reset_at": "t"},
                }}]}),
            ),
            set(&[
                ("plan_stale", "items"),
                ("plan_stale", "items[].error"),
                ("plan_stale", "items[].error.code"),
                ("plan_stale", "items[].error.details"),
                ("plan_stale", "items[].error.details.reset_at"),
                ("plan_stale", "items[].request"),
                ("plan_stale", "items[].target"),
                ("rate_limited", "reset_at"),
            ]),
        ),
        (
            "an_unregistered_code_is_no_error",
            json!({"ok": true, "data": {"code": "not_a_code", "details": {"x": 1}}}),
            set(&[]),
        ),
        (
            "no_error",
            json!({"ok": true, "data": {"items": []}}),
            set(&[]),
        ),
    ];
    for (name, envelope, expected) in cases {
        assert_eq!(
            observed_details(&envelope),
            expected,
            "case {name}: {envelope}"
        );
    }
}
