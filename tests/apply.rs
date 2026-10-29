//! e2e suite: stored previews, bounded admission, concurrency and crash recovery; and bound runs that commit exactly what their preview approved, or nothing (M4-D60): the approval witness and the re-judged operations checked again under the commit lock, and each rename's identity.
#![cfg(test)]

#[path = "apply/commands.rs"]
mod commands;
mod common;
#[path = "apply/witness.rs"]
mod witness;

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use jiff::{SignedDuration, Timestamp};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use ulid::Ulid;

use common::home::Home;
use common::pause::Pause;
use common::plans::{
    age_plan, edit_plan, lease_files, plan_files, plans_dir, plant_expired, plant_live, read_plan,
    store_listing,
};
use common::tlog::{Ran, TestLog, tlog};
use omaddons::error::ErrorCode;
use omaddons::limits::{PLAN_MAX_BYTES, PLANS_MAX_BYTES, PLANS_MAX_FILES, PLANS_SWEEP_MAX};
use omaddons::state::plans::load;

const NOW: &str = "2026-10-27T12:00:00Z";

fn home(log: &TestLog) -> Home {
    log.step("prepare home");
    let home = Home::new(&["wow"]);
    fs::write(
        home.env.root().join("foo.zip"),
        common::archives::zip_text(&[
            (
                "Foo/Foo.toc",
                "## Interface: 120105\n## Version: 1\nFoo.lua\n",
            ),
            ("Foo/Foo.lua", "local foo = true\n"),
        ]),
    )
    .unwrap();
    log.env(&home);
    home
}

fn preview_args(home: &Home, name: &str) -> Vec<String> {
    vec![
        "install".into(),
        format!("gh:o/{name}"),
        "--file".into(),
        home.env.root().join("foo.zip").display().to_string(),
        "-t".into(),
        "retail".into(),
        "--no-deps".into(),
        "--offline".into(),
        "--dry-run".into(),
        "--json".into(),
        "--progress".into(),
        "json".into(),
        "-v".into(),
    ]
}

fn preview(log: &TestLog, home: &Home, name: &str) -> Ran {
    let ran = log
        .cmd(home, &preview_args(home, name))
        .env("OMADDONS_NOW", NOW)
        .run();
    ran.ok().assert_private();
    ran
}

fn template(log: &TestLog, home: &Home) -> Value {
    log.step("make a valid template and empty the plan files");
    let ran = preview(log, home, "foo");
    let raw = read_plan(&home.env, ran.data()["plan_id"].as_str().unwrap());
    log.json("template", &raw);
    for path in plan_files(&home.env) {
        fs::remove_file(path).unwrap();
    }
    assert_eq!(store_listing(&home.env).0, 0);
    raw
}

fn listing(log: &TestLog, home: &Home) -> (usize, usize, u64) {
    let listing = store_listing(&home.env);
    let now: Timestamp = NOW.parse().unwrap();
    let expired = plan_files(&home.env)
        .iter()
        .filter_map(|path| load(path).ok())
        .filter(|plan| plan.expires_at <= now)
        .count();
    log.json(
        "store",
        &json!({"plans": listing.0, "leases": listing.1, "bytes": listing.2, "expired": expired}),
    );
    listing
}

fn hashes(home: &Home) -> BTreeMap<String, String> {
    plan_files(&home.env)
        .into_iter()
        .map(|path| {
            (
                path.file_name().unwrap().to_string_lossy().into_owned(),
                Sha256::digest(fs::read(&path).unwrap())
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect(),
            )
        })
        .collect()
}

fn not_stored(log: &TestLog, ran: &Ran) {
    ran.ok();
    log.assert_eq("no stored plan", &ran.data()["plan_id"], &Value::Null);
    log.assert_eq(
        "no plan expiry",
        &ran.data()["plan_expires_at"],
        &Value::Null,
    );
    log.assert_eq(
        "one plan warning",
        &ran.warnings(),
        &vec!["plan_not_stored".to_owned()],
    );
    assert!(
        ran.data()["precondition"]
            .as_str()
            .unwrap()
            .starts_with("pc1:")
    );
    let events: Vec<_> = ran
        .events()
        .into_iter()
        .filter(|event| event["event"] == "warning")
        .collect();
    log.assert_eq("one matching stderr warning", &events.len(), &1);
    log.assert_eq(
        "stderr warning code",
        &events[0]["code"],
        &json!("plan_not_stored"),
    );
}

#[test]
fn store_hand_edited_plans_fail_closed() {
    let log = tlog!();
    let home = home(&log);
    log.step("store original plan");
    let ran = preview(&log, &home, "foo");
    let original_id = ran.data()["plan_id"].as_str().unwrap();
    let original = read_plan(&home.env, original_id);
    for case in [
        "truncated",
        "oversized",
        "unknown_variant",
        "unknown_option",
        "relative_path",
        "display_argv",
        "token",
        "newer_version",
        "expiry",
        "plan_id",
    ] {
        log.step(&format!("edit plan {case}"));
        let id = Ulid::generate();
        age_plan(&home.env, &original, id, NOW.parse().unwrap());
        let id = id.to_string();
        let path = plans_dir(&home.env).join(format!("{id}.json"));
        match case {
            "truncated" => fs::write(&path, b"{\"schema_version\":").unwrap(),
            "oversized" => fs::write(
                &path,
                vec![b' '; usize::try_from(PLAN_MAX_BYTES + 1).unwrap()],
            )
            .unwrap(),
            _ => edit_plan(&home.env, &id, |plan| match case {
                "unknown_variant" => plan["intent"]["command"] = json!("unknown"),
                "unknown_option" => plan["intent"]["unknown_option"] = json!(true),
                "relative_path" => plan["intent"]["file"] = json!("./foo.zip"),
                "display_argv" => plan["display_argv"][1] = json!("remove"),
                "token" => plan["precondition"]["token"] = json!("pc1:edited"),
                "newer_version" => plan["schema_version"] = json!("99.0"),
                "expiry" => {
                    plan["expires_at"] = json!(
                        NOW.parse::<Timestamp>()
                            .unwrap()
                            .checked_add(SignedDuration::from_hours(25))
                            .unwrap()
                            .to_string()
                    )
                }
                "plan_id" => plan["plan_id"] = json!(Ulid::generate().to_string()),
                _ => {}
            }),
        }
        log.step("load refuses the edited copy");
        let error = load(&path).unwrap_err();
        log.json(
            "refusal",
            &json!({"code":error.code.wire(), "message":error.message, "details":error.details}),
        );
        assert_eq!(error.code, ErrorCode::PlanNotFound, "{case}: {error:?}");
        assert_eq!(error.details["reason"], "unreadable", "{case}: {error:?}");
        assert_eq!(
            error.details["path"],
            path.display().to_string(),
            "{case}: {error:?}"
        );
        assert!(
            load(&plans_dir(&home.env).join(format!("{original_id}.json"))).is_ok(),
            "{case}: original still loads"
        );
        assert_eq!(
            read_plan(&home.env, original_id),
            original,
            "{case}: original unchanged"
        );
    }
}

struct RestoreMode {
    path: PathBuf,
    mode: u32,
}
impl Drop for RestoreMode {
    fn drop(&mut self) {
        let _ = fs::set_permissions(&self.path, fs::Permissions::from_mode(self.mode));
    }
}

#[test]
fn store_a_write_failure_warns_and_the_preview_stands() {
    let log = tlog!();
    for case in ["read_only_directory", "admission_io_error"] {
        let home = home(&log);
        let _ = template(&log, &home);
        log.step(case);
        let dir = plans_dir(&home.env);
        let guard = (case == "read_only_directory").then(|| {
            let mode = fs::metadata(&dir).unwrap().permissions().mode();
            fs::set_permissions(&dir, fs::Permissions::from_mode(0o500)).unwrap();
            RestoreMode { path: dir, mode }
        });
        let mut command = log
            .cmd(&home, &preview_args(&home, "new"))
            .env("OMADDONS_NOW", NOW);
        if case == "admission_io_error" {
            command = command.failpoints("plans.admit.locked=error");
        }
        let ran = command.run();
        drop(guard);
        not_stored(&log, &ran);
        log.assert_eq("no plan files", &listing(&log, &home).0, &0);
    }
}

#[test]
fn quota_600_expired_plans_converge() {
    let log = tlog!();
    let home = home(&log);
    let raw = template(&log, &home);
    log.step("plant 600 expired plans");
    plant_expired(&home.env, &raw, 600);
    log.note("planted 600 expired, 0 live");
    let mut previous = listing(&log, &home).0;
    for run in 0..10 {
        log.step(&format!("dry run {run}"));
        let ran = preview(&log, &home, "foo");
        let count = listing(&log, &home).0;
        if run == 0 {
            not_stored(&log, &ran);
            assert_eq!(count, 536);
        } else {
            assert!(ran.data()["plan_id"].is_string());
            assert!(ran.warnings().is_empty());
        }
        let inserted = usize::from(run == 1);
        let removed = previous + inserted - count;
        assert!(removed <= PLANS_SWEEP_MAX, "run {run}: removed {removed}");
        if run == 1 {
            assert_eq!(count, 473);
        }
        if run > 0 {
            assert!(count <= PLANS_MAX_FILES);
        }
        previous = count;
    }
    log.assert_eq("one deduplicated live plan", &listing(&log, &home).0, &1);
}

#[test]
fn quota_a_full_store_declines_and_evicts_nothing() {
    let log = tlog!();
    for case in ["count", "bytes"] {
        let home = home(&log);
        let raw = template(&log, &home);
        log.step(&format!("plant live plans to {case} quota"));
        if case == "count" {
            plant_live(&home.env, &raw, PLANS_MAX_FILES, 0);
        } else {
            let target = usize::try_from(PLAN_MAX_BYTES - 1024).unwrap();
            let budget = PLANS_MAX_BYTES - 1024;
            let mut padded = raw.clone();
            padded["summary"].as_array_mut().unwrap().push(json!(""));
            let base = serde_json::to_vec(&padded).unwrap().len();
            let n = usize::try_from(budget / u64::try_from(target).unwrap()).unwrap();
            plant_live(&home.env, &raw, n, target - base);
            let remainder = budget - store_listing(&home.env).2;
            plant_live(
                &home.env,
                &raw,
                1,
                usize::try_from(remainder).unwrap() - base,
            );
            assert_eq!(store_listing(&home.env).2, budget);
            assert!(store_listing(&home.env).0 < PLANS_MAX_FILES);
            assert!(serde_json::to_vec(&raw).unwrap().len() > 1024);
        }
        let before = hashes(&home);
        log.json("planted plan hashes", &before);
        listing(&log, &home);
        log.step("new intent is declined without eviction");
        let ran = preview(&log, &home, "new");
        not_stored(&log, &ran);
        let after = hashes(&home);
        log.json("remaining plan hashes", &after);
        log.assert_eq("every live file byte-identical", &after, &before);
    }
}

#[test]
fn quota_concurrent_dry_runs_stay_within_the_quotas() {
    let log = tlog!();
    let home = home(&log);
    let raw = template(&log, &home);
    log.step("plant 508 live plans and race eight distinct intents");
    plant_live(&home.env, &raw, PLANS_MAX_FILES - 4, 0);
    let runs: Vec<_> = (0..8)
        .map(|index| {
            log.cmd(&home, &preview_args(&home, &format!("race{index}")))
                .env("OMADDONS_NOW", NOW)
                .spawn()
        })
        .collect();
    let results: Vec<_> = runs.into_iter().map(|run| run.wait()).collect();
    let stored = results
        .iter()
        .filter(|run| {
            run.ok();
            run.data()["plan_id"].is_string()
        })
        .count();
    log.assert_eq("four admitted", &stored, &4);
    for run in results.iter().filter(|run| run.data()["plan_id"].is_null()) {
        not_stored(&log, run);
    }
    let held = listing(&log, &home);
    log.assert_eq("count quota", &held.0, &PLANS_MAX_FILES);
    assert!(
        held.2 <= PLANS_MAX_BYTES,
        "concurrent previews stayed within byte quota: {held:?}"
    );

    let home = self::home(&log);
    let raw = template(&log, &home);
    plant_live(&home.env, &raw, PLANS_MAX_FILES - 1, 0);
    log.step("A holds admission while B queues");
    let pause = Pause::new(home.env.root());
    let a = log
        .cmd(&home, &preview_args(&home, "a"))
        .env("OMADDONS_NOW", NOW)
        .failpoints("plans.admit.locked=pause")
        .pause(&pause)
        .spawn();
    log.reached(&pause, "plans.admit.locked");
    let b = log
        .cmd(&home, &preview_args(&home, "b"))
        .env("OMADDONS_NOW", NOW)
        .spawn();
    log.resume(&pause, "plans.admit.locked");
    let a = a.wait();
    let b = b.wait();
    a.ok();
    assert!(a.data()["plan_id"].is_string());
    not_stored(&log, &b);
    let held = listing(&log, &home);
    log.assert_eq("serialized admission count", &held.0, &PLANS_MAX_FILES);
    assert!(
        held.2 <= PLANS_MAX_BYTES,
        "serialized admission stayed within byte quota: {held:?}"
    );
}

#[test]
fn quota_an_oversized_plan_file_is_never_read_or_swept() {
    let log = tlog!();
    let home = home(&log);
    let raw = template(&log, &home);
    plant_expired(&home.env, &raw, 11);
    let oversized = plan_files(&home.env).remove(0);
    fs::write(
        &oversized,
        vec![b' '; usize::try_from(PLAN_MAX_BYTES + 1).unwrap()],
    )
    .unwrap();
    let before = fs::read(&oversized).unwrap();
    log.note("planted one oversized expired file and ten readable expired plans");
    for index in 0..3 {
        log.step(&format!("preview and inspect {index}"));
        let ran = preview(&log, &home, "foo");
        assert!(ran.data()["plan_id"].is_string());
        let held = listing(&log, &home);
        log.assert_eq("oversized counts beside live plan", &held.0, &2);
        assert!(held.2 > PLAN_MAX_BYTES);
        assert_eq!(fs::read(&oversized).unwrap(), before);
        let error = load(&oversized).unwrap_err();
        assert_eq!(error.code, ErrorCode::PlanNotFound);
        assert_eq!(error.details["reason"], "unreadable");
        assert!(
            error.message.contains("larger than 256 KiB"),
            "metadata refuses before JSON decoding: {error:?}"
        );
    }
}

#[test]
fn crash_while_the_admission_lock_is_held() {
    let log = tlog!();
    let home = home(&log);
    log.step("abort while admission holds the lease");
    let killed = log
        .cmd(&home, &preview_args(&home, "foo"))
        .env("OMADDONS_NOW", NOW)
        .failpoints("plans.admit.locked=abort")
        .run();
    killed.aborted();
    assert!(killed.run.stdout.is_empty());
    log.step("kernel releases the lease for the next preview");
    let next = log
        .cmd(&home, &preview_args(&home, "foo"))
        .env("OMADDONS_NOW", NOW)
        .timeout(Duration::from_secs(1))
        .run();
    next.ok();
    assert!(next.data()["plan_id"].is_string());
    log.assert_eq("one plan", &listing(&log, &home).0, &1);
}

#[test]
fn crash_after_the_plan_was_written() {
    let log = tlog!();
    let home = home(&log);
    log.step("abort after durable plan write");
    let killed = log
        .cmd(&home, &preview_args(&home, "foo"))
        .env("OMADDONS_NOW", NOW)
        .failpoints("plans.admit.written=abort")
        .run();
    killed.aborted();
    let files = plan_files(&home.env);
    log.assert_eq("one written plan", &files.len(), &1);
    let stored = load(&files[0]).unwrap();
    log.json("written plan", &serde_json::to_value(&stored).unwrap());
    log.step("same preview reuses the surviving plan");
    let next = preview(&log, &home, "foo");
    log.assert_eq(
        "same id",
        &next.data()["plan_id"],
        &json!(stored.plan_id.to_string()),
    );
    log.assert_eq("one plan after dedupe", &listing(&log, &home).0, &1);
}

fn empty(dir: &Path) -> bool {
    !dir.exists() || fs::read_dir(dir).unwrap().next().is_none()
}

#[test]
fn crash_between_a_sweeps_renames() {
    let log = tlog!();
    let home = home(&log);
    let raw = template(&log, &home);
    plant_expired(&home.env, &raw, 5);
    for plan in plan_files(&home.env) {
        fs::write(plan.with_extension("lease"), b"").unwrap();
    }
    log.step("abort after first expired pair moves to trash");
    let killed = log
        .cmd(&home, &preview_args(&home, "foo"))
        .env("OMADDONS_NOW", NOW)
        .failpoints("fsx.prune.renamed=abort@1")
        .run();
    killed.aborted();
    let trash = home.env.cache().join("omaddons/trash");
    assert!(!empty(&trash));
    listing(&log, &home);
    log.step("repeated previews converge without orphan leases");
    for _ in 0..3 {
        preview(&log, &home, "foo");
        listing(&log, &home);
    }
    log.assert_eq(
        "only one live plan remains",
        &plan_files(&home.env).len(),
        &1,
    );
    log.assert_eq("no orphan leases", &lease_files(&home.env).len(), &0);
    log.step("global-lock retention drains the trash");
    // A fresh install has no cache victims by default. Give its retention an old,
    // dead cache-side lock so it performs the global prune that sweeps this side.
    let orphan = home
        .env
        .cache()
        .join("omaddons/inflight")
        .join(format!("{}.lock", Ulid::generate()));
    fs::create_dir_all(orphan.parent().unwrap()).unwrap();
    fs::write(&orphan, b"").unwrap();
    let old: std::time::SystemTime = NOW
        .parse::<Timestamp>()
        .unwrap()
        .checked_sub(SignedDuration::from_hours(2))
        .unwrap()
        .into();
    fs::File::open(&orphan).unwrap().set_modified(old).unwrap();
    log.json("cache orphan eligible for retention", &orphan);
    let mut argv = preview_args(&home, "foo");
    argv.retain(|arg| arg != "--dry-run");
    log.cmd(&home, &argv).env("OMADDONS_NOW", NOW).run().ok();
    log.tree("trash after retention", &trash);
    assert!(!orphan.exists(), "retention performed its cache-side prune");
    assert!(empty(&trash));
}

/// `apply` (D5): a stored install preview applied once commits exactly the previewed
/// folders and records the plan in history; applied again, it is `already_applied`.
#[test]
fn apply_smoke_install_plan_round_trip() {
    let log = tlog!();
    let home = home(&log);
    let addons = home.addons("_retail_");

    log.step("preview");
    let ran = preview(&log, &home, "foo");
    let id = ran.data()["plan_id"].as_str().unwrap().to_owned();
    let plan = read_plan(&home.env, &id);
    log.json("plan file", &plan);
    log.tree("addons", &addons);

    log.step("apply");
    let applied = log
        .cmd(
            &home,
            &["apply", id.as_str(), "--json", "--progress", "json"],
        )
        .env("OMADDONS_NOW", NOW)
        .run();
    applied.ok().assert_private();
    let data = applied.data();
    log.assert_eq(
        "applied command",
        &data["applied"]["command"],
        &json!("install"),
    );
    log.assert_eq(
        "outcome",
        &data["outcomes"][0]["status"],
        &json!("committed"),
    );
    let tx = data["result"]["tx"].clone();
    log.assert_eq("outcome tx", &data["outcomes"][0]["tx"], &tx);
    let diff = log.diff("addons", &addons);
    assert!(
        diff.removed.is_empty() && diff.changed.is_empty() && !diff.added.is_empty(),
        "{diff:?}"
    );
    assert!(
        diff.added.iter().all(|path| path.starts_with("Foo")),
        "only Foo's folders: {diff:?}"
    );
    log.tree("addons", &addons);

    log.step("history");
    let history = log
        .cmd(&home, &["history", "--json"])
        .env("OMADDONS_NOW", NOW)
        .run();
    history.ok();
    let newest = &history.data()["entries"][0];
    log.json("newest history line", newest);
    log.assert_eq("plan", &newest["plan"], &json!(id));
    log.assert_eq("argv", &newest["argv"], &plan["display_argv"]);
    log.assert_eq("tx", &newest["tx"], &tx);

    log.step("apply again");
    let again = log
        .cmd(
            &home,
            &["apply", id.as_str(), "--json", "--progress", "json"],
        )
        .env("OMADDONS_NOW", NOW)
        .run();
    again.fails(1, "plan_stale");
    let details = &again.envelope()["error"]["details"];
    log.assert_eq("reason", &details["reason"], &json!("already_applied"));
    log.assert_eq("txs", &details["txs"], &json!([tx]));
    log.assert_unchanged("addons", &addons);
}

/// A stored `undo --run R -t retail` keeps its client (omad-bjiw): the plan names
/// retail's transaction of the run and binds as `undo <tx>` does, and applying it
/// reverts retail alone, leaving the run's other client as it was.
#[test]
fn apply_an_undo_of_one_client_of_a_run() {
    let log = tlog!();
    log.step("prepare a home with retail and Mists Classic");
    let home = Home::new(&["wow", "wow_classic"]);
    let zip = home.env.root().join("foo.zip");
    fs::write(
        &zip,
        common::archives::zip_text(&[(
            "Foo/Foo.toc",
            "## Interface: 120105, 50504\n## Version: 1\n",
        )]),
    )
    .unwrap();
    log.env(&home);
    let zip = zip.display().to_string();
    let run = |args: &[&str]| {
        let mut argv: Vec<&str> = args.to_vec();
        argv.extend(["--offline", "--json", "--progress", "json"]);
        let ran = log.cmd(&home, &argv).env("OMADDONS_NOW", NOW).run();
        ran.ok().assert_private();
        ran
    };
    for target in ["retail", "classic"] {
        log.step(&format!("install Foo on {target}"));
        run(&["install", "gh:o/foo", "--file", &zip, "-t", target]);
    }

    log.step("pin Foo on both clients: one run, a transaction on each");
    let pinned = run(&["pin", "Foo"]);
    let data = pinned.data();
    let run_id = data["run"].as_str().unwrap().to_owned();
    let tx_of = |id: &str| -> String {
        let target = data["targets"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["target"]["id"] == id)
            .unwrap_or_else(|| panic!("no {id} in {data}")); // ubs:ignore — the test's own failure
        target["tx"].as_str().unwrap().to_owned()
    };
    let (retail_tx, classic_tx) = (tx_of("retail"), tx_of("classic"));
    log.json(
        "the run",
        &json!({"run": run_id, "retail": retail_tx, "classic": classic_tx}),
    );
    assert_ne!(retail_tx, classic_tx);

    log.step("preview undo --run R -t retail");
    let preview = run(&["undo", "--run", &run_id, "-t", "retail", "--dry-run"]);
    let id = preview.data()["plan_id"].as_str().unwrap().to_owned();
    let token = preview.data()["precondition"].clone();
    log.assert_eq(
        "the preview still names the run",
        &preview.data()["reverted_run"],
        &json!(run_id),
    );
    let plan = read_plan(&home.env, &id);
    log.json("plan file", &plan);
    log.assert_eq(
        "stored selection",
        &plan["intent"]["selection"],
        &json!({"tx": retail_tx}),
    );
    log.assert_eq(
        "display argv",
        &plan["display_argv"],
        &json!(["omaddons", "undo", retail_tx]),
    );

    log.step("undo of retail's transaction by its id binds alike");
    let direct = run(&["undo", &retail_tx, "--dry-run"]);
    log.assert_eq("token", &direct.data()["precondition"], &token);
    log.assert_eq(
        "the same intent and token reuse the plan",
        &direct.data()["plan_id"],
        &json!(id),
    );

    log.step("apply it: retail's pin is undone, classic's stays");
    let applied = run(&["apply", &id]);
    let data = applied.data();
    log.assert_eq(
        "applied command",
        &data["applied"]["command"],
        &json!("undo"),
    );
    let outcomes: Vec<(Value, Value)> = data["outcomes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| (o["target"].clone(), o["status"].clone()))
        .collect();
    log.assert_eq(
        "outcomes",
        &outcomes,
        &vec![(json!("retail"), json!("committed"))],
    );
    log.assert_eq(
        "the reverted transaction",
        &data["result"]["reverted"]["tx"],
        &json!(retail_tx),
    );
    let listed = run(&["list"]);
    let pinned_on = |id: &str| -> Value {
        listed.data()["targets"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["id"] == id)
            .and_then(|t| t["addons"].as_array())
            .and_then(|addons| addons.iter().find(|a| a["name"] == "Foo"))
            .map(|foo| foo["pinned"].clone())
            .unwrap_or(Value::Null)
    };
    log.assert_eq(
        "Foo pinned (retail, classic)",
        &(pinned_on("retail"), pinned_on("classic")),
        &(json!(false), json!(true)),
    );
}
