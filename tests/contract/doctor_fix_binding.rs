//! `doctor fix` binds (M4-D44, common rule 16): a preview in one process and a bound run
//! in another agree, `doctor diff` offers the same token, and a stale token refuses with
//! the preview as its fix, changing nothing outside the doctor's own run record.

use serde_json::{Value, json};

use super::*;
use crate::common::tlog::{Ran, TestLog, tlog};

/// Runs `argv` through the whole contract at `exit`.
#[track_caller]
fn checked(log: &TestLog, s: &support::Scene, argv: &[&str], exit: i32) -> Ran {
    let ran = log.run(&s.home, argv);
    support::check(ran.to_run(), exit);
    ran
}

/// The run's `precondition`, a token.
#[track_caller]
fn token_of(log: &TestLog, label: &str, ran: &Ran) -> String {
    let token = ran.envelope()["precondition"].clone();
    log.json(label, &token);
    let token = token.as_str().unwrap_or_default().to_owned();
    assert!(
        omaddons::app::precondition::is_token(&token),
        "{label}: {token:?}"
    );
    token
}

#[test]
fn preview_then_bound_run_then_stale() {
    let s = support::Scene::new();
    let log = tlog!();
    log.env(&s.home);
    let state = s.home.env.state();

    log.step("preview");
    let preview = checked(&log, &s, &["doctor", "fix", "--dry-run", "--json"], 0);
    log.assert_eq("dry_run", &preview.envelope()["dry_run"], &json!(true));
    let token = token_of(&log, "preview token", &preview);

    log.step("bound run");
    let bound = checked(
        &log,
        &s,
        &["doctor", "fix", "--if-match", &token, "--json"],
        0,
    );
    log.assert_eq("dry_run", &bound.envelope()["dry_run"], &json!(false));
    log.assert_eq("state", &bound.envelope()["state"], &json!("DONE_OK"));
    log.assert_eq(
        "precondition",
        &bound.envelope()["precondition"],
        &Value::Null,
    );

    log.step("diff");
    let diff = checked(&log, &s, &["doctor", "diff", "--json"], 0);
    let diffs = token_of(&log, "diff token", &diff);
    let again = checked(&log, &s, &["doctor", "fix", "--dry-run", "--json"], 0);
    let fresh = token_of(&log, "fresh preview token", &again);
    log.assert_eq("diff token", &json!(diffs), &json!(fresh));
    log.assert_eq("unchanged home", &json!(fresh), &json!(token));

    log.step("stale");
    // One hex digit changed: well formed, and no longer what the home is.
    let mut stale = token.clone();
    let last = stale.pop().unwrap_or('0');
    // ubs:ignore — changing a test token's last digit, not comparing a secret
    stale.push(if last == '0' { '1' } else { '0' });
    log.json("stale token", &stale);
    log.tree("state", &state);
    let refused = checked(
        &log,
        &s,
        &["doctor", "fix", "--if-match", &stale, "--json"],
        4,
    );
    let error = &refused.envelope()["error"];
    log.assert_eq("error.code", &error["code"], &json!("plan_stale"));
    log.assert_eq("error.retryable", &error["retryable"], &json!(false));
    log.assert_eq(
        "error.fix_argv",
        &error["fix_argv"],
        &json!(["omaddons", "doctor", "fix", "--dry-run"]),
    );
    assert!(
        error["details"]["changed"]
            .as_array()
            .is_some_and(|changed| !changed.is_empty()),
        "{error}"
    );
    log.assert_eq("state", &refused.envelope()["state"], &json!("REFUSING"));
    // Only the refused run's own record changed, and the global lock's holder line,
    // which every command that takes the lock rewrites: the token is compared under it.
    let changed = log.diff("state", &state);
    let outside: Vec<String> = changed
        .first(changed.total)
        .into_iter()
        .filter(|line| {
            let path = &line[2..];
            !path.starts_with("omaddons/doctor/") && path != "omaddons/lock (content)"
        })
        .collect();
    log.assert_eq(
        "changed outside the doctor's record",
        &outside,
        &Vec::<String>::new(),
    );
}
