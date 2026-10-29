//! Ordinary stored previews through the real CLI, with every invocation logged.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use jiff::{SignedDuration, Timestamp};
use omaddons::app::intent::StoredIntent;
use omaddons::backup::BackupKind;
use omaddons::state::plans::{StoredPlan, load};
use serde_json::{Value, json};

use crate::common::home::Home;
use crate::common::pause::Pause;
use crate::common::plans::{plan_files, plans_dir, store_listing};
use crate::common::tlog::{Cmd, Ran, TestLog};
use crate::common::{self, state};

pub(crate) const NOW: &str = "2026-10-27T12:00:00Z";
const YESTERDAY: &str = "2026-10-26T12:00:00Z";
const FLAVORS: &[(&str, u32)] = &[("mainline", 120_105), ("mists", 50_504)];

pub(crate) fn strings(args: &[&str]) -> Vec<String> {
    args.iter().map(|arg| (*arg).to_owned()).collect()
}

pub(crate) fn command<'a>(log: &'a TestLog, home: &Home, args: &[String], json: bool) -> Cmd<'a> {
    let mut argv = args.to_vec();
    if json {
        argv.push("--json".to_owned());
    }
    argv.extend(strings(&["--progress", "json"]));
    log.cmd(home, &argv).env("OMADDONS_NOW", NOW)
}

pub(crate) fn run(log: &TestLog, home: &Home, args: &[String]) -> Ran {
    let ran = command(log, home, args, true).run();
    ran.ok().assert_private();
    ran
}

pub(crate) fn preview(log: &TestLog, home: &Home, args: &[String]) -> Ran {
    let mut argv = args.to_vec();
    argv.push("--dry-run".to_owned());
    run(log, home, &argv)
}

fn entries(name: &str, version: &str) -> Vec<(String, String)> {
    vec![
        (
            format!("{name}/{name}.toc"),
            format!(
                "## Interface: 120105,50504\n## Title: {name}\n## Version: {version}\n## X-Website: https://github.com/o/{}\n{name}.lua\n",
                name.to_ascii_lowercase()
            ),
        ),
        (
            format!("{name}/{name}.lua"),
            format!("local version = {version}\n"),
        ),
    ]
}

pub(crate) fn serve(home: &Home, name: &str, version: &str) {
    let entries = entries(name, version);
    let borrowed: Vec<_> = entries
        .iter()
        .map(|(path, text)| (path.as_str(), text.as_str()))
        .collect();
    let filename = format!("{name}.zip");
    common::http::gh_release(
        &home.http,
        &format!("https://github.com/o/{}", name.to_ascii_lowercase()),
        &format!("v{version}"),
        &[(filename.as_str(), FLAVORS, borrowed)],
    );
}

pub(crate) fn zip(home: &Home, name: &str, version: &str) -> PathBuf {
    let entries = entries(name, version);
    let borrowed: Vec<_> = entries
        .iter()
        .map(|(path, text)| (path.as_str(), text.as_str()))
        .collect();
    let path = home.env.root().join(format!("{name}-{version}.zip"));
    fs::write(&path, common::archives::zip_text(&borrowed)).unwrap();
    path
}

pub(crate) fn unmanaged(home: &Home, name: &str) {
    let entries = entries(name, "1");
    let borrowed: Vec<_> = entries
        .iter()
        .map(|(path, text)| (path.as_str(), text.as_str()))
        .collect();
    common::addons::unpack(&home.addons("_retail_"), &borrowed);
}

pub(crate) fn install(log: &TestLog, home: &Home, name: &str, now: &str) -> Ran {
    let args = strings(&[
        "install",
        &format!("gh:o/{}", name.to_ascii_lowercase()),
        "-t",
        "retail",
    ]);
    let ran = command(log, home, &args, true)
        .env("OMADDONS_NOW", now)
        .run();
    ran.ok().assert_private();
    ran
}

fn backup(home: &Home, at: &str) -> String {
    let mut meta = common::backups::meta(BackupKind::Manual, at.parse().unwrap(), 1);
    meta.target = state::key_of(home, "retail");
    common::backups::seed_meta(&home.client("_retail_"), &meta).to_string()
}

fn old_backups(home: &Home) -> Vec<String> {
    [
        "2026-10-23T08:00:00Z",
        "2026-10-24T08:00:00Z",
        "2026-10-25T08:00:00Z",
    ]
    .into_iter()
    .map(|at| backup(home, at))
    .collect()
}

fn listing(log: &TestLog, home: &Home) {
    let (plans, leases, bytes) = store_listing(&home.env);
    let now: Timestamp = NOW.parse().unwrap();
    let expired = plan_files(&home.env)
        .iter()
        .filter_map(|path| load(path).ok())
        .filter(|plan| plan.expires_at <= now)
        .count();
    log.json(
        "store",
        &json!({"plans": plans, "leases": leases, "bytes": bytes, "expired": expired}),
    );
}

fn stored(log: &TestLog, home: &Home, ran: &Ran) -> StoredPlan {
    ran.ok().assert_private();
    let id = ran.data()["plan_id"]
        .as_str()
        .expect("a changing preview stores a plan");
    assert!(
        id.parse::<ulid::Ulid>().is_ok(),
        "invalid plan id in {}",
        ran.data()
    );
    let path = plans_dir(&home.env).join(format!("{id}.json"));
    let plan = load(&path).unwrap();
    log.json("stored plan", &plan);
    listing(log, home);
    log.assert_eq(
        "owner-only plan mode",
        &(fs::metadata(path).unwrap().permissions().mode() & 0o777),
        &0o600,
    );
    log.assert_eq(
        "returned token stored",
        &json!(plan.precondition.token),
        &ran.data()["precondition"],
    );
    log.assert_eq(
        "returned approval stored",
        &serde_json::to_value(&plan.approval).unwrap(),
        &ran.data()["approval"],
    );
    log.assert_eq(
        "returned expiry stored",
        &json!(plan.expires_at),
        &ran.data()["plan_expires_at"],
    );
    log.assert_eq(
        "24 hour TTL",
        &plan.expires_at.duration_since(plan.created_at).as_secs(),
        &SignedDuration::from_hours(24).as_secs(),
    );
    plan
}

/// A home seeded for one storable command, the arguments of its change, and the
/// `display_argv` its stored plan must carry.
pub(crate) struct Fixture {
    pub(crate) home: Home,
    pub(crate) args: Vec<String>,
    pub(crate) expected: Vec<String>,
}

pub(crate) fn home_for(log: &TestLog, path: &str) -> Fixture {
    log.step("prepare home");
    let home = Home::new(&["wow", "wow_classic"]);
    log.env(&home);
    serve(&home, "Foo", "1");
    let (args, expected) = match path {
        "install" => {
            let file = zip(&home, "Foo", "1").display().to_string();
            (
                strings(&["install", "gh:o/foo", "--file", &file, "-t", "retail"]),
                strings(&[
                    "omaddons", "install", "gh:o/foo", "-t", "retail", "--file", &file,
                ]),
            )
        }
        "update" => {
            install(log, &home, "Foo", YESTERDAY);
            serve(&home, "Foo", "2");
            (
                strings(&["update", "-t", "retail", "--refresh"]),
                strings(&["omaddons", "update", "-t", "retail", "--refresh"]),
            )
        }
        "pin" | "unpin" => {
            install(log, &home, "Foo", NOW);
            if path == "unpin" {
                run(log, &home, &strings(&["pin", "Foo", "-t", "retail"]));
            }
            (
                strings(&[path, "Foo", "-t", "retail"]),
                strings(&["omaddons", path, "Foo", "-t", "retail"]),
            )
        }
        "adopt" => {
            serve(&home, "Bar", "1");
            unmanaged(&home, "Bar");
            (
                strings(&["adopt", "--apply", "-t", "retail"]),
                strings(&["omaddons", "adopt", "--apply", "-t", "retail"]),
            )
        }
        "sync" => {
            let file = home
                .write_desired(
                    "schema_version = \"1.0\"\n[target.retail]\naddons = [\"gh:o/foo\"]\n",
                )
                .display()
                .to_string();
            (
                strings(&["sync", &file, "-t", "retail"]),
                strings(&["omaddons", "sync", &file, "-t", "retail"]),
            )
        }
        "copy" => {
            install(log, &home, "Foo", NOW);
            (
                strings(&["copy", "--from", "retail", "--to", "classic"]),
                strings(&["omaddons", "copy", "--from", "retail", "--to", "classic"]),
            )
        }
        "remove" => {
            install(log, &home, "Foo", NOW);
            (
                strings(&["remove", "Foo", "-t", "retail"]),
                strings(&["omaddons", "remove", "Foo", "-t", "retail"]),
            )
        }
        "undo" => {
            install(log, &home, "Foo", NOW);
            let run = state::history(&home)
                .last()
                .unwrap()
                .run
                .unwrap()
                .to_string();
            (
                strings(&["undo"]),
                strings(&["omaddons", "undo", "--run", &run]),
            )
        }
        "backup restore" => {
            install(log, &home, "Foo", NOW);
            common::wtf::Wtf::at(home.client("_retail_"))
                .saved_variables("Foo", 64)
                .build();
            let id = backup(&home, "2026-10-24T08:00:00Z");
            (
                strings(&["backup", "restore", &id, "-t", "retail"]),
                strings(&["omaddons", "backup", "restore", &id, "-t", "retail"]),
            )
        }
        "backup prune" => {
            install(log, &home, "Foo", NOW);
            old_backups(&home);
            let cutoff =
                (NOW.parse::<Timestamp>().unwrap() - SignedDuration::from_hours(24)).to_string();
            (
                strings(&["backup", "prune", "--older-than", "1d", "-t", "retail"]),
                strings(&[
                    "omaddons",
                    "backup",
                    "prune",
                    "--older-than",
                    &cutoff,
                    "-t",
                    "retail",
                ]),
            )
        }
        other => panic!("missing fixture for {other}"), // ubs:ignore — intentional test-only failure for an unhandled storable command
    };
    Fixture {
        home,
        args,
        expected,
    }
}

#[test]
fn store_every_storable_dry_run_writes_its_plan() {
    let log = crate::common::tlog::tlog!();
    for (path, _) in StoredIntent::PATHS {
        let log = log.case(path);
        let fixture = home_for(&log, path);
        log.step("warm artifacts before comparing stored and human summaries");
        preview(&log, &fixture.home, &fixture.args);
        // A preview may report download/cache counts. Compare both renderings from
        // the same warm cache, without reusing the cold preview's saved summary.
        for file in plan_files(&fixture.home.env) {
            fs::remove_file(file).unwrap();
        }
        log.step("preview and inspect stored plan");
        let ran = preview(&log, &fixture.home, &fixture.args);
        let plan = stored(&log, &fixture.home, &ran);
        log.assert_eq("canonical intent path", &plan.intent.command_path(), &path);
        log.assert_eq(
            "normalized display argv",
            &plan.display_argv,
            &fixture.expected,
        );
        log.assert_eq(
            "creation clock",
            &plan.created_at.to_string(),
            &NOW.to_owned(),
        );

        log.step("human preview agrees with stored summary");
        let mut args = fixture.args.clone();
        args.push("--dry-run".to_owned());
        let human = command(&log, &fixture.home, &args, false).run();
        human.ok().assert_private();
        let lines: Vec<String> = human
            .run
            .stdout
            .lines()
            .filter(|line| !line.starts_with("Plan "))
            .map(str::to_owned)
            .collect();
        log.assert_eq(
            "summary is the human preview without its plan line",
            &plan.summary,
            &lines,
        );
        listing(&log, &fixture.home);
        log.assert_eq(
            "same human preview deduplicates",
            &plan_files(&fixture.home.env).len(),
            &1,
        );
    }
}

#[test]
fn store_a_no_op_dry_run_writes_no_plan() {
    let log = crate::common::tlog::tlog!();
    for path in [
        "install",
        "update",
        "pin",
        "unpin",
        "adopt",
        "sync",
        "copy",
        "backup prune",
    ] {
        let log = log.case(path);
        let fixture = home_for(&log, path);
        log.step("complete the change before previewing it again");
        run(&log, &fixture.home, &fixture.args);
        let before = plan_files(&fixture.home.env);
        log.json("plan files before no-op", &before);
        log.step("preview the no-op");
        let ran = preview(&log, &fixture.home, &fixture.args);
        log.assert_eq("no plan id", &ran.data()["plan_id"], &Value::Null);
        log.assert_eq(
            "no plan expiry",
            &ran.data()["plan_expires_at"],
            &Value::Null,
        );
        log.assert_eq(
            "empty approval selection",
            &ran.data()["approval"]["selection"],
            &json!([]),
        );
        log.assert_eq("no warning", &ran.warnings(), &Vec::<String>::new());
        log.assert_eq("no plan written", &plan_files(&fixture.home.env), &before);
        listing(&log, &fixture.home);
    }
}

#[test]
fn store_real_runs_carry_null_plan_keys() {
    let log = crate::common::tlog::tlog!();
    let home = Home::new(&["wow"]);
    log.env(&home);
    serve(&home, "Foo", "1");
    for path in ["install", "update", "remove", "undo"] {
        log.step(&format!("real {path}"));
        let args = match path {
            "install" => strings(&["install", "gh:o/foo", "-t", "retail"]),
            "update" => {
                serve(&home, "Foo", "2");
                strings(&["update", "--refresh", "-t", "retail"])
            }
            "remove" => strings(&["remove", "Foo", "-t", "retail"]),
            "undo" => strings(&["undo"]),
            _ => unreachable!(),
        };
        let before = plan_files(&home.env);
        let ran = run(&log, &home, &args);
        for key in ["plan_id", "plan_expires_at", "approval"] {
            log.assert_eq(
                &format!("{path}: {key} is null"),
                &ran.data()[key],
                &Value::Null,
            );
        }
        log.assert_eq("real run writes no plan", &plan_files(&home.env), &before);
        listing(&log, &home);
    }
}

#[test]
fn store_relative_inputs_are_stored_absolute() {
    let log = crate::common::tlog::tlog!();
    for path in ["install", "sync", "adopt", "update"] {
        let log = log.case(path);
        let fixture = home_for(&log, path);
        let scratch = fixture.home.env.root().join("scratch");
        fs::create_dir_all(&scratch).unwrap();
        let (args, expected) = match path {
            "install" => {
                fs::copy(zip(&fixture.home, "Foo", "1"), scratch.join("foo.zip")).unwrap();
                (
                    strings(&["install", "gh:o/foo", "--file", "./foo.zip", "-t", "retail"]),
                    scratch.join("foo.zip"),
                )
            }
            "sync" => {
                fs::write(
                    scratch.join("addons.toml"),
                    "schema_version = \"1.0\"\n[target.retail]\naddons = [\"gh:o/foo\"]\n",
                )
                .unwrap();
                (
                    strings(&["sync", "./addons.toml", "-t", "retail"]),
                    scratch.join("addons.toml"),
                )
            }
            "adopt" => {
                let entries = entries("Bar", "1");
                let borrowed: Vec<_> = entries
                    .iter()
                    .map(|(path, text)| (path.as_str(), text.as_str()))
                    .collect();
                common::addons::unpack(&scratch, &borrowed);
                (
                    strings(&[
                        "adopt",
                        "Bar",
                        "--ref",
                        "./Bar",
                        "-t",
                        "retail",
                        "--no-verify",
                    ]),
                    scratch.join("Bar"),
                )
            }
            "update" => {
                let client = fixture.home.client("_retail_");
                let relative = format!(
                    "../{}",
                    client
                        .strip_prefix(fixture.home.env.root())
                        .unwrap()
                        .display()
                );
                (strings(&["update", "-t", &relative, "--refresh"]), client)
            }
            _ => unreachable!(),
        };
        log.step("preview from a scratch working directory");
        let mut args = args;
        args.push("--dry-run".to_owned());
        let ran = command(&log, &fixture.home, &args, true)
            .cwd(&scratch)
            .run();
        ran.ok();
        let plan = stored(&log, &fixture.home, &ran);
        let actual = match &plan.intent {
            StoredIntent::Install(intent) => intent.file.as_ref().unwrap().as_str(),
            StoredIntent::Sync(intent) => intent.file.as_str(),
            StoredIntent::Adopt(intent) => intent.r#ref.as_ref().unwrap().file().unwrap().as_str(),
            StoredIntent::Update(intent) => intent.scope.selector.as_deref().unwrap(),
            _ => unreachable!(),
        };
        assert!(Path::new(actual).is_absolute(), "{path}: {actual}");
        log.assert_eq(
            "canonical absolute input",
            &actual.to_owned(),
            &expected.canonicalize().unwrap().display().to_string(),
        );
        log.step("remove the old working directory and load the plan again");
        fs::remove_dir_all(&scratch).unwrap();
        let loaded =
            load(&plans_dir(&fixture.home.env).join(format!("{}.json", plan.plan_id))).unwrap();
        log.assert_eq("loading is independent of the old cwd", &loaded, &plan);
    }
}

#[test]
fn store_a_repeated_preview_returns_the_same_plan() {
    let log = crate::common::tlog::tlog!();
    let fixture = home_for(&log, "install");
    log.step("first preview at noon");
    let first = preview(&log, &fixture.home, &fixture.args);
    let original = stored(&log, &fixture.home, &first);
    log.step("repeat an hour later with different attribution");
    let mut args = fixture.args.clone();
    args.extend(strings(&[
        "--dry-run",
        "--actor",
        "agent",
        "--reason",
        "another preview",
    ]));
    let repeated = command(&log, &fixture.home, &args, true)
        .env("OMADDONS_NOW", "2026-10-27T13:00:00Z")
        .run();
    repeated.ok();
    log.assert_eq(
        "same plan id",
        &repeated.data()["plan_id"],
        &first.data()["plan_id"],
    );
    log.assert_eq(
        "original expiry retained",
        &repeated.data()["plan_expires_at"],
        &json!("2026-10-28T12:00:00Z"),
    );
    log.assert_eq(
        "only one stored preview",
        &plan_files(&fixture.home.env).len(),
        &1,
    );
    listing(&log, &fixture.home);
    log.step("another committed addon invalidates the old preview");
    serve(&fixture.home, "Other", "1");
    install(&log, &fixture.home, "Other", NOW);
    let changed = preview(&log, &fixture.home, &fixture.args);
    let changed = stored(&log, &fixture.home, &changed);
    assert_ne!(
        changed.plan_id, original.plan_id,
        "new generation must create a new plan"
    );
}

#[test]
fn store_human_output_names_the_plan() {
    let log = crate::common::tlog::tlog!();
    let fixture = home_for(&log, "install");
    log.step("JSON preview reveals the id");
    let json_run = preview(&log, &fixture.home, &fixture.args);
    let plan = stored(&log, &fixture.home, &json_run);
    log.step("human preview names the same plan and lifetime");
    let mut args = fixture.args.clone();
    args.push("--dry-run".to_owned());
    let human = command(&log, &fixture.home, &args, false).run();
    human.ok();
    let final_line = human.run.stdout.lines().last().unwrap();
    log.assert_eq(
        "plan footer",
        &final_line.to_owned(),
        &format!("Plan {} stored for 24 hours.", plan.plan_id),
    );
    listing(&log, &fixture.home);
    log.step("after installation a no-op has no footer");
    run(&log, &fixture.home, &fixture.args);
    let noop = command(&log, &fixture.home, &args, false).run();
    noop.ok();
    assert!(
        !noop
            .run
            .stdout
            .lines()
            .any(|line| line.starts_with("Plan ")),
        "{}",
        noop.run.stdout
    );
    listing(&log, &fixture.home);
}

#[test]
fn store_dry_runs_leave_state_untouched_and_take_no_global_lock() {
    let log = crate::common::tlog::tlog!();
    log.step("prepare the union of all eleven commands' prerequisites");
    let home = Home::new(&["wow", "wow_classic"]);
    log.env(&home);
    serve(&home, "Foo", "1");
    let installed = install(&log, &home, "Foo", YESTERDAY);
    let undo_tx = installed.data()["tx"].as_str().unwrap().to_owned();
    serve(&home, "Foo", "2");
    serve(&home, "Bar", "1");
    install(&log, &home, "Bar", NOW);
    run(&log, &home, &strings(&["pin", "Bar", "-t", "retail"]));
    serve(&home, "Baz", "1");
    unmanaged(&home, "Baz");
    common::wtf::Wtf::at(home.client("_retail_"))
        .saved_variables("Foo", 64)
        .build();
    let backups = old_backups(&home);
    let install_file = zip(&home, "New", "1").display().to_string();
    let sync_file = zip(&home, "Desired", "1").display().to_string();
    let desired = home
        .write_desired(&format!(
            "schema_version = \"1.0\"\n[target.retail]\naddons = [\"file:{sync_file}\"]\n"
        ))
        .display()
        .to_string();
    let cases = [
        (
            "install",
            strings(&[
                "install",
                "gh:o/new",
                "--file",
                &install_file,
                "-t",
                "retail",
            ]),
        ),
        ("update", strings(&["update", "Foo", "-t", "retail"])),
        ("pin", strings(&["pin", "Foo", "-t", "retail"])),
        ("unpin", strings(&["unpin", "Bar", "-t", "retail"])),
        ("adopt", strings(&["adopt", "--apply", "-t", "retail"])),
        ("sync", strings(&["sync", &desired, "-t", "retail"])),
        (
            "copy",
            strings(&["copy", "--from", "retail", "--to", "classic", "Foo"]),
        ),
        ("remove", strings(&["remove", "Foo", "-t", "retail"])),
        (
            "undo",
            strings(&["undo", &undo_tx, "--saved-variables", "keep"]),
        ),
        (
            "backup restore",
            strings(&["backup", "restore", &backups[0], "-t", "retail"]),
        ),
        (
            "backup prune",
            strings(&["backup", "prune", "--older-than", "1d", "-t", "retail"]),
        ),
    ];
    log.step("warm network inputs before comparing the entire state including net.log");
    for (_, args) in &cases {
        preview(&log, &home, args);
    }
    let held_file = zip(&home, "Holder", "1").display().to_string();
    let pause = Pause::new(home.env.root());
    let holder_args = strings(&[
        "install",
        "gh:o/holder",
        "--file",
        &held_file,
        "-t",
        "retail",
        "--offline",
    ]);
    log.step("hold the global commit lock in another process");
    let holder = command(&log, &home, &holder_args, true)
        .failpoints("engine.lock.acquired=pause")
        .pause(&pause)
        .spawn();
    log.reached(&pause, "engine.lock.acquired");
    let before = log.tree("state while holder is paused", &home.env.state());
    let mut results = Vec::new();
    for (path, args) in &cases {
        log.step(&format!(
            "{path} stores its preview while the global lock is held"
        ));
        let mut args = args.clone();
        args.extend(strings(&["--offline", "--dry-run"]));
        let ran = command(&log, &home, &args, true)
            .timeout(Duration::from_secs(2))
            .run();
        results.push((path, ran));
        listing(&log, &home);
    }
    let after = log.tree("state after the eleven previews", &home.env.state());
    // Release the holder before asserting, so a failure never waits for its timeout.
    log.resume(&pause, "engine.lock.acquired");
    holder.wait().ok();
    log.assert_eq(
        "all eleven previews left state hash-identical",
        &after,
        &before,
    );
    for (path, ran) in results {
        ran.ok().assert_private();
        assert!(ran.elapsed_ms < 2_000, "{path}: took {} ms", ran.elapsed_ms);
        assert!(ran.data()["plan_id"].is_string(), "{path}: {}", ran.data());
        assert!(
            !ran.data()["approval"]["selection"]
                .as_array()
                .unwrap()
                .is_empty(),
            "{path}: {}",
            ran.data()
        );
    }
}
