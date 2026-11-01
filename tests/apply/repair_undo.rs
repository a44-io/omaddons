//! E7 s7 (omad-m4-agent-surface-doctor-c6cm.5.7.7): the undo of a repair through a stored
//! plan. `undo <repair tx> --dry-run` stores a plan whose token is its precondition, and
//! `apply <plan>` reverts exactly that repair (or refuses it, stale), its history line
//! carrying `plan`, also when it was crashed after `done` and rolled forward.

use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use jiff::{SignedDuration, Timestamp};
use serde_json::{Value, json};

use crate::common::archives::zip_text;
use crate::common::doctor::plant;
use crate::common::doctor::test_fixers::{
    ENV, FLATTEN, REPAIRED, Side, WRITE, plant_flatten, plant_write, register, test_dir,
};
use crate::common::home::Home;
use crate::common::plans::{edit_plan, plans_dir, read_plan};
use crate::common::tlog::{Cmd, Ran, TestLog, tlog};

/// `omaddons <args> --json --progress json` with the test fixers registered.
fn cmd<'a>(log: &'a TestLog, home: &Home, args: &[&str]) -> Cmd<'a> {
    let mut argv: Vec<&str> = args.to_vec();
    argv.extend(["--json", "--progress", "json"]);
    log.cmd(home, &argv)
        .env(ENV.0, ENV.1)
        .env("OMARCHY_PATH", plant::omarchy_path(home))
}

fn run(log: &TestLog, home: &Home, args: &[&str]) -> Ran {
    let ran = cmd(log, home, args).run();
    ran.assert_private();
    ran
}

/// A home with one committed `fm-test-write` repair.
struct Repaired {
    home: Home,
    /// The repaired file.
    file: PathBuf,
    /// The repair's transaction.
    tx: String,
}

fn repaired(log: &TestLog) -> Repaired {
    log.step("repair");
    let home = Home::new(&["wow"]);
    log.env(&home);
    let file = plant_write(&home, "a.write", b"broken\n");
    log.tree("subject", &test_dir(&home, Side::State));
    let fixed = run(log, &home, &["doctor", "fix", "--only", WRITE]);
    fixed.ok();
    let tx = fixed.envelope()["tx"].as_str().unwrap().to_owned();
    assert_eq!(fs::read(&file).unwrap(), REPAIRED, "repaired");
    Repaired { home, file, tx }
}

/// A stored preview: its plan id, its token and the plan file.
struct Preview {
    id: String,
    token: String,
    plan: Value,
}

fn preview(log: &TestLog, home: &Home, tx: &str) -> Preview {
    log.step("preview");
    let ran = run(log, home, &["undo", tx, "--dry-run"]);
    ran.ok();
    let data = ran.data();
    let id = data["plan_id"].as_str().unwrap().to_owned();
    let token = data["precondition"].as_str().unwrap().to_owned();
    let plan = read_plan(&home.env, &id);
    log.json("plan", &plan);
    assert_eq!(
        plan["precondition"]["token"],
        json!(token),
        "the plan binds the token"
    );
    Preview { id, token, plan }
}

/// The plan `p` refused as stale for `reason`, before anything moved.
fn refused(log: &TestLog, ran: &Ran, reason: &str) {
    ran.fails(1, "plan_stale").assert_private();
    let error = &ran.envelope()["error"];
    log.assert_eq("reason", &error["details"]["reason"], &json!(reason));
    log.assert_eq("retryable", &error["retryable"], &json!(false));
}

/// History's lines reverting `tx`.
fn undo_lines(log: &TestLog, home: &Home, tx: &str) -> Vec<Value> {
    run(log, home, &["history"]).ok().data()["entries"]
        .as_array()
        .unwrap()
        .iter()
        // ubs:ignore — comparing history kinds and transaction ids, not secrets
        .filter(|e| e["kind"] == "undo" && e["reverts"] == tx)
        .cloned()
        .collect()
}

#[test]
fn undo_of_a_repair_previews_stores_and_applies() {
    let log = tlog!();
    let r = repaired(&log);
    let p = preview(&log, &r.home, &r.tx);
    assert_eq!(
        p.plan["intent"]["selection"],
        json!({ "tx": r.tx }),
        "the stored intent names the repair, not a target"
    );

    log.step("apply");
    let applied = run(&log, &r.home, &["apply", &p.id]);
    applied.ok();
    let data = applied.data();
    log.assert_eq("the undo's kind", &data["result"]["kind"], &json!("repair"));
    log.assert_eq(
        "committed",
        &data["outcomes"][0]["status"],
        &json!("committed"),
    );

    log.step("assert");
    log.assert_unchanged("subject", &test_dir(&r.home, Side::State));
    let lines = undo_lines(&log, &r.home, &r.tx);
    log.assert_eq("one undo line", &lines.len(), &1);
    log.assert_eq("its plan", &lines[0]["plan"], &json!(p.id));

    log.step("plans show agrees");
    let shown = run(&log, &r.home, &["plans", "show", &p.id]);
    shown.ok();
    let data = shown.data();
    log.assert_eq("completed", &data["state"], &json!("completed"));
    log.assert_eq(
        "the machine committed it",
        &data["evidence"]["targets"][0]["key"],
        &Value::Null,
    );
    log.assert_eq(
        "named as the machine",
        &data["evidence"]["targets"][0]["target"],
        &json!("this machine"),
    );
}

#[test]
fn undo_of_a_repair_is_stale_after_a_change() {
    let log = tlog!();
    {
        let log = log.case("the repaired file edited");
        let r = repaired(&log);
        let p = preview(&log, &r.home, &r.tx);
        fs::write(&r.file, b"edited\n").unwrap();
        log.tree("state", &r.home.env.state());
        let ran = run(&log, &r.home, &["apply", &p.id]);
        ran.fails(1, "plan_stale");
        let changed = &ran.envelope()["error"]["details"]["changed"];
        assert!(
            changed.as_array().unwrap().contains(&json!("observed")),
            "{changed}"
        );
        log.assert_unchanged("state", &r.home.env.state());
        assert_eq!(fs::read(&r.file).unwrap(), b"edited\n", "nothing moved");
    }
    {
        let log = log.case("an install on the repair's client");
        log.step("repair");
        let home = Home::new(&["wow"]);
        log.env(&home);
        register(&home, "retail");
        plant_flatten(&home, "_retail_", "TestWrapA", "Inner");
        let fixed = run(&log, &home, &["doctor", "fix", "--only", FLATTEN]);
        fixed.ok();
        let tx = fixed.envelope()["tx"].as_str().unwrap().to_owned();
        let p = preview(&log, &home, &tx);

        log.step("install on retail");
        let dl = home.env.root().join("dl");
        fs::create_dir_all(&dl).unwrap();
        let zip = dl.join("foo.zip");
        fs::write(
            &zip,
            zip_text(&[("Foo/Foo.toc", "## Interface: 120105\n## Version: 1\n")]),
        )
        .unwrap();
        let zip = zip.display().to_string();
        run(
            &log,
            &home,
            &["install", "gh:o/foo", "--file", &zip, "-t", "retail"],
        )
        .ok();
        let trees = [
            ("state", home.env.state()),
            ("client", home.client("_retail_")),
        ];
        for (label, root) in &trees {
            log.tree(label, root);
        }

        log.step("apply");
        let ran = run(&log, &home, &["apply", &p.id]);
        ran.fails(1, "plan_stale");
        let changed = &ran.envelope()["error"]["details"]["changed"];
        assert!(
            changed.as_array().unwrap().contains(&json!("targets")),
            "{changed}"
        );
        for (label, root) in &trees {
            log.assert_unchanged(label, root);
        }
    }
}

#[test]
fn undo_of_a_repair_plan_lifecycle() {
    let log = tlog!();
    {
        let log = log.case("expired");
        let r = repaired(&log);
        let p = preview(&log, &r.home, &r.tx);
        let expires: Timestamp = p.plan["expires_at"].as_str().unwrap().parse().unwrap();
        let late = (expires + SignedDuration::from_mins(1)).to_string();
        let ran = cmd(&log, &r.home, &["apply", &p.id])
            .env("OMADDONS_NOW", &late)
            .run();
        refused(&log, &ran, "expired");
    }
    {
        let log = log.case("binary_changed");
        let r = repaired(&log);
        let p = preview(&log, &r.home, &r.tx);
        edit_plan(&r.home.env, &p.id, |plan| {
            plan["build_id"] = json!("0000000000000000");
        });
        refused(
            &log,
            &run(&log, &r.home, &["apply", &p.id]),
            "binary_changed",
        );
    }
    {
        let log = log.case("truncated");
        let r = repaired(&log);
        let p = preview(&log, &r.home, &r.tx);
        let path = plans_dir(&r.home.env).join(format!("{}.json", p.id));
        let bytes = fs::read(&path).unwrap();
        fs::write(&path, &bytes[..bytes.len() / 2]).unwrap();
        let ran = run(&log, &r.home, &["apply", &p.id]);
        ran.fails(3, "plan_not_found");
        log.assert_eq(
            "reason",
            &ran.envelope()["error"]["details"]["reason"],
            &json!("unreadable"),
        );
        run(&log, &r.home, &["apply", "nosuchid0"]).fails(3, "plan_not_found");
    }
    {
        let log = log.case("dry run, then twice");
        let r = repaired(&log);
        let p = preview(&log, &r.home, &r.tx);

        log.step("apply --dry-run under the global lock and the plan's lease");
        let paths = crate::common::http::paths(&r.home.env);
        let global = omaddons::fsx::GlobalLock::acquire(&paths, "test").unwrap();
        let id: omaddons::model::PlanId = p.id.parse().unwrap();
        let scopes = omaddons::fsx::ScopeTable::new(&paths, &[], None).unwrap();
        let lease = omaddons::fsx::Lease::acquire(
            &scopes,
            &paths.plan_lease(&id),
            "test",
            Duration::from_secs(1),
        )
        .map_err(|_| "the plan's lease")
        .unwrap();
        log.tree("state", &r.home.env.state());
        log.tree("plans", &plans_dir(&r.home.env));
        let started = Instant::now();
        let dry = cmd(&log, &r.home, &["apply", &p.id, "--dry-run"])
            .timeout(Duration::from_secs(20))
            .run();
        dry.ok();
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "it took no lock"
        );
        log.assert_eq("approved", &dry.data()["approved"], &p.plan["approval"]);
        log.assert_unchanged("state", &r.home.env.state());
        log.assert_unchanged("plans", &plans_dir(&r.home.env));
        drop(lease);
        drop(global);

        log.step("apply, then again");
        let first = run(&log, &r.home, &["apply", &p.id]);
        first.ok();
        let undo_tx = first.data()["outcomes"][0]["tx"].clone();
        let again = run(&log, &r.home, &["apply", &p.id]);
        refused(&log, &again, "already_applied");
        log.assert_eq(
            "the first undo's transaction",
            &again.envelope()["error"]["details"]["txs"],
            &json!([undo_tx]),
        );
        log.assert_eq("the token", &p.token.starts_with("pc1:"), &true);
    }
}

#[test]
fn an_applied_undo_crashed_after_done_keeps_its_plan() {
    let log = tlog!();
    let r = repaired(&log);
    let p = preview(&log, &r.home, &r.tx);

    log.step("apply, crashed after done");
    cmd(&log, &r.home, &["apply", &p.id])
        .failpoints("doctor.repair.history=abort")
        .run()
        .aborted();

    log.step("recover");
    run(
        &log,
        &r.home,
        &["config", "set", "check_interval_hours", "6"],
    )
    .ok();
    let lines = undo_lines(&log, &r.home, &r.tx);
    log.assert_eq("one undo line", &lines.len(), &1);
    log.assert_eq("its plan", &lines[0]["plan"], &json!(p.id));
    log.assert_unchanged("subject", &test_dir(&r.home, Side::State));

    log.step("apply again");
    refused(
        &log,
        &run(&log, &r.home, &["apply", &p.id]),
        "already_applied",
    );
}
