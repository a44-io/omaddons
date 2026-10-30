//! Two appliers of one plan, the lease's timeout, another command committing between an
//! `apply`'s targets, and a lease a running `apply` holds against both plan sweeps
//! (D7's races, M4-D61). Every interleaving is set by `pause` markers, and a waiting
//! applier is found by its open lease file, never by a sleep.

use std::fs;
use std::time::{Duration, Instant};

use jiff::{SignedDuration, Timestamp};
use serde_json::{Value, json};

use crate::common::http::paths;
use crate::common::pause::Pause;
use crate::common::plans::{
    evidence_txs, history_txs_for, holds_open, lease_files, lease_path, plan_files, plan_id,
    plans_dir, read_plan, show, wait_until_open,
};
use crate::common::tlog::{Ran, TestLog, tlog};

use race_support::{RaceHome, cmd, first_committed, preview_both, race_home};

/// Hit after each target's commit window has unlocked and cleaned up, before the next
/// target starts: a pause there holds no global lock.
const CLEANUP: &str = "engine.cleanup.done";
/// Hit once, when every target is planned and before any commit: an applier that slipped
/// past the lease would plan, and stop here.
const PLANNED: &str = "engine.plan.done";

mod race_support {
    use std::fs;

    use serde_json::Value;

    use crate::common::archives::zip_text;
    use crate::common::home::Home;
    use crate::common::http::Asset;
    use crate::common::plans::{history_txs_for, plan_id, read_plan};
    use crate::common::tlog::{Cmd, TestLog};

    const FOO: &str = "https://github.com/o/foo";
    const FLAVORS: &[(&str, u32)] = &[("mainline", 120_105), ("mists", 50_504)];
    const FOO_V1: &[(&str, &str)] = &[
        (
            "Foo/Foo.toc",
            "## Interface: 120105, 50504\n## Title: Foo\n## Version: v1\ncore.lua\n",
        ),
        ("Foo/core.lua", "-- Foo v1\n"),
    ];
    const FOO_V2: &[(&str, &str)] = &[
        (
            "Foo/Foo.toc",
            "## Interface: 120105, 50504\n## Title: Foo\n## Version: v2\ncore.lua\n",
        ),
        ("Foo/core.lua", "-- Foo v2\n"),
    ];

    /// Foo's release as one GitHub asset, declared for retail and Mists Classic.
    fn foo(entries: &'static [(&'static str, &'static str)]) -> [Asset<'static>; 1] {
        [("Foo.zip", FLAVORS, entries.to_vec())]
    }

    /// A retail and a Mists Classic client, and a local archive of another addon.
    pub struct RaceHome {
        pub home: Home,
        /// `<root>/dl/bar.zip`: `Bar`, which both clients load.
        pub bar_zip: String,
    }

    /// Foo v1 of `gh:o/foo` installed on `targets` while v2 is its latest release, the
    /// release memo a day old; and `bar_zip`.
    pub fn race_home(log: &TestLog, targets: &[&str]) -> RaceHome {
        log.step(&format!(
            "seed home: Foo v1 on {}, v2 published",
            targets.join(", ")
        ));
        let mut home = Home::new(&["wow", "wow_classic"]);
        log.env(&home);
        home.installed_behind(FOO, ("v1", &foo(FOO_V1)), ("v2", &foo(FOO_V2)), targets);
        let dl = home.env.root().join("dl");
        fs::create_dir_all(&dl).unwrap();
        let bar = dl.join("bar.zip");
        fs::write(
            &bar,
            zip_text(&[(
                "Bar/Bar.toc",
                "## Interface: 120105, 50504\n## Title: Bar\n## Version: 1\n",
            )]),
        )
        .unwrap();
        RaceHome {
            home,
            bar_zip: bar.display().to_string(),
        }
    }

    /// `omaddons <args> --json --progress json` on the real clock: the release memo
    /// `Home::installed_behind` wrote carries real times.
    pub fn cmd<'a>(log: &'a TestLog, home: &Home, args: &[&str]) -> Cmd<'a> {
        let mut argv = args.to_vec();
        argv.extend(["--json", "--progress", "json"]);
        log.cmd(home, &argv)
    }

    /// `update foo --dry-run`, which updates Foo to v2 on both clients: the plan's id and
    /// its file.
    pub fn preview_both(log: &TestLog, home: &Home) -> (String, Value) {
        let ran = cmd(log, home, &["update", "foo", "--dry-run"]).run();
        ran.ok().assert_private();
        let mut updates: Vec<(String, String, String)> = ran.data()["targets"]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|target| {
                let id = target["target"]["id"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned();
                target["items"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(move |item| {
                        let field = |key: &str| item[key].as_str().unwrap_or_default().to_owned();
                        (id.clone(), field("name"), field("to_version"))
                    })
            })
            .collect();
        updates.sort();
        let both =
            ["classic", "retail"].map(|id| (id.to_owned(), "Foo".to_owned(), "v2".to_owned()));
        log.assert_eq("Foo to v2 on both clients", &updates, &both.to_vec());
        let id = plan_id(&ran);
        let plan = read_plan(&home.env, &id);
        log.json("plan file", &plan);
        (id, plan)
    }

    /// The one target history records for plan `id` so far: `(target id, tx)`.
    pub fn first_committed(log: &TestLog, home: &Home, id: &str) -> (String, String) {
        let lines = history_txs_for(log, home, id);
        log.assert_eq("one target committed so far", &lines.len(), &1);
        lines[0].clone()
    }
}

/// The client that is not `id`.
fn other(id: &str) -> String {
    if id == "retail" { "classic" } else { "retail" }.to_owned()
}

/// `id`'s client folder.
fn folder(id: &str) -> &'static str {
    match id {
        "retail" => "_retail_",
        "classic" => "_classic_",
        _ => unreachable!("no client {id} in the race home"),
    }
}

/// Foo's `core.lua` on client `id`: it names the installed release.
fn core(race: &RaceHome, id: &str) -> String {
    let path = race.home.addons(folder(id)).join("Foo/core.lua");
    fs::read_to_string(path).unwrap_or_default()
}

/// Each `outcomes[]` entry, as `{target, status, tx, error_code}`, in order.
fn outcomes(log: &TestLog, ran: &Ran) -> Vec<Value> {
    let outcomes: Vec<Value> = ran.data()["outcomes"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|o| {
            json!({
                "target": o["target"],
                "status": o["status"],
                "tx": o["tx"],
                "error_code": o["error_code"],
            })
        })
        .collect();
    log.json("outcomes", &outcomes);
    outcomes
}

/// A committed outcome on `target` with `tx`.
fn committed_on(target: &str, tx: &str) -> Value {
    json!({"target": target, "status": "committed", "tx": tx, "error_code": null})
}

/// A refusal that names a plan already applied, with exactly `txs`.
#[track_caller]
fn already_applied(log: &TestLog, ran: &Ran, txs: &[&str]) {
    ran.fails(1, "plan_stale").assert_private();
    let details = &ran.envelope()["error"]["details"];
    log.json("refusal details", details);
    log.assert_eq("reason", &details["reason"], &json!("already_applied"));
    log.assert_eq(
        "its transactions, oldest first",
        &details["txs"],
        &json!(txs),
    );
}

/// `history.jsonl`'s line count.
fn history_lines(race: &RaceHome) -> usize {
    fs::read_to_string(paths(&race.home.env).history_file())
        .map(|text| text.lines().count())
        .unwrap_or(0)
}

/// The waiting applier's sentinel markers: whether it reached [`PLANNED`] or [`CLEANUP`].
/// When either is there it slipped past the lease: both are resumed, so it exits, and
/// the test fails.
#[track_caller]
fn never_planned(log: &TestLog, waiter: &Pause) {
    let reached: Vec<(&str, bool)> = [PLANNED, CLEANUP]
        .map(|name| (name, waiter.was_reached(name)))
        .to_vec();
    log.note(format!("the waiting applier's markers: {reached:?}"));
    let slipped = reached.iter().any(|(_, at)| *at);
    if slipped {
        for name in [PLANNED, CLEANUP] {
            log.resume(waiter, name);
        }
    }
    log.assert_eq(
        "the second applier planned or committed while the first held the lease",
        &slipped,
        &false,
    );
}

/// Whether process `pid` has ended: gone, or a zombie the test has not reaped yet.
fn exited(pid: u32) -> bool {
    match fs::read_to_string(format!("/proc/{pid}/stat")) {
        Err(_) => true,
        Ok(stat) => stat
            .rsplit_once(')')
            .and_then(|(_, rest)| rest.split_whitespace().next())
            .is_none_or(|state| matches!(state, "Z" | "X")),
    }
}

/// Watches the waiting applier `pid` until it ends (30 s at most), checking its sentinel
/// markers on every poll, so one that slipped past the lease fails the test at once
/// instead of sitting paused.
#[track_caller]
fn until_it_ends(log: &TestLog, waiter: &Pause, pid: u32) {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if [PLANNED, CLEANUP]
            .iter()
            .any(|name| waiter.was_reached(name))
        {
            never_planned(log, waiter);
        }
        if exited(pid) {
            log.note(format!("pid {pid} ended"));
            return;
        }
        if Instant::now() >= deadline {
            log.note(format!("pid {pid} still runs after 30 s"));
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn race_split_two_appliers_never_split_a_plan() {
    let log = tlog!();
    let race = race_home(&log, &["retail", "classic"]);
    let home = &race.home;
    log.step("seed and preview");
    let (p, _) = preview_both(&log, home);
    let lease = lease_path(&home.env, &p);

    log.step("first applier commits its first target");
    let pa = Pause::new(home.env.root());
    let a = cmd(&log, home, &["apply", &p])
        .failpoints(&format!("{CLEANUP}=pause"))
        .pause(&pa)
        .spawn();
    log.reached(&pa, CLEANUP);
    let (x, t1) = first_committed(&log, home, &p);
    let y = other(&x);

    log.step("second applier waits on the lease");
    let pb = Pause::new(home.env.root());
    let b = cmd(&log, home, &["apply", &p])
        .failpoints(&format!("{PLANNED}=pause,{CLEANUP}=pause"))
        .pause(&pb)
        .spawn();
    wait_until_open(&log, b.pid(), &lease, Duration::from_secs(10));
    // Only a slipped applier's chance to show itself; the checks after both end bind.
    std::thread::sleep(Duration::from_millis(200));
    never_planned(&log, &pb);

    log.step("first applier finishes");
    log.resume(&pa, CLEANUP);
    let a = a.wait();
    a.ok().assert_private();
    let got = outcomes(&log, &a);
    let t2 = got
        .iter()
        .find(|o| o["target"] == y.as_str())
        .and_then(|o| o["tx"].as_str())
        .unwrap_or_default()
        .to_owned();
    assert!(
        !t2.is_empty() && t2 != t1,
        "two distinct transactions: {got:?}"
    );
    let mut expected = vec![committed_on(&x, &t1), committed_on(&y, &t2)];
    expected.sort_by_key(|o| o["target"].as_str().unwrap_or_default().to_owned());
    log.assert_eq("both targets committed by the first", &got, &expected);

    log.step("second applier reports");
    until_it_ends(&log, &pb, b.pid());
    let b = b.wait();
    already_applied(&log, &b, &[t1.as_str(), t2.as_str()]);
    log.assert_eq(
        "it waited for the lease, never for its timeout (elapsed_ms < 30000)",
        &(b.elapsed_ms < 30_000),
        &true,
    );
    never_planned(&log, &pb);

    log.step("evidence");
    log.assert_eq(
        "history carries the plan twice",
        &history_txs_for(&log, home, &p),
        &vec![(x.clone(), t1.clone()), (y.clone(), t2.clone())],
    );
    let shown = show(&log, home, &[&p], &[]);
    shown.ok();
    log.json("plans show", shown.data());
    log.assert_eq("state", &shown.data()["state"], &json!("completed"));
    log.assert_eq(
        "evidence",
        &evidence_txs(&shown),
        &vec![(x.clone(), t1), (y.clone(), t2)],
    );
    log.assert_eq(
        "complete evidence",
        &shown.data()["evidence"]["complete"],
        &json!(true),
    );
    for id in [&x, &y] {
        log.assert_eq(
            &format!("Foo on {id}"),
            &core(&race, id),
            &"-- Foo v2\n".to_owned(),
        );
    }
}

#[test]
fn race_split_the_waiter_proceeds_when_the_first_committed_nothing() {
    let log = tlog!();
    let race = race_home(&log, &["retail", "classic"]);
    let home = &race.home;
    log.step("seed and preview");
    let (p, _) = preview_both(&log, home);
    let lease = lease_path(&home.env, &p);

    log.step("first applier holds the lease and fails");
    let pa = Pause::new(home.env.root());
    let a = cmd(&log, home, &["apply", &p])
        .failpoints(&format!("{PLANNED}=pause,engine.lock.acquired=error"))
        .pause(&pa)
        .spawn();
    log.reached(&pa, PLANNED);

    log.step("second applier waits");
    // Held where it has planned but committed nothing: it gets there only through the
    // lease, and the first applier's empty record is checked before it commits.
    let pb = Pause::new(home.env.root());
    let b = cmd(&log, home, &["apply", &p])
        .failpoints(&format!("{PLANNED}=pause"))
        .pause(&pb)
        .spawn();
    wait_until_open(&log, b.pid(), &lease, Duration::from_secs(10));

    log.step("first applier commits nothing");
    log.resume(&pa, PLANNED);
    let a = a.wait();
    log.reached(&pb, PLANNED);
    if a.envelope().get("data").is_some() {
        log.note("the first applier's shape: exit 0, partial, every outcome failed");
        a.ok().assert_private();
        log.assert_eq("partial", &a.envelope()["partial"], &json!(true));
        let got = outcomes(&log, &a);
        let failed: Vec<Value> = ["classic", "retail"]
            .map(|id| json!({"target": id, "status": "failed", "tx": null, "error_code": "io"}))
            .to_vec();
        log.assert_eq("every outcome failed", &got, &failed);
    } else {
        log.note("the first applier's shape: the update failed as a whole");
        a.fails(1, "io").assert_private();
    }
    log.assert_eq(
        "no history line carries the plan",
        &history_txs_for(&log, home, &p),
        &Vec::new(),
    );
    for id in ["retail", "classic"] {
        log.assert_eq(
            &format!("Foo on {id}"),
            &core(&race, id),
            &"-- Foo v1\n".to_owned(),
        );
    }

    log.step("second applier proceeds");
    log.resume(&pb, PLANNED);
    let b = b.wait();
    b.ok().assert_private();
    let got = outcomes(&log, &b);
    let statuses: Vec<(Value, Value)> = got
        .iter()
        .map(|o| (o["target"].clone(), o["status"].clone()))
        .collect();
    log.assert_eq(
        "both committed",
        &statuses,
        &vec![
            (json!("classic"), json!("committed")),
            (json!("retail"), json!("committed")),
        ],
    );
    let mut reported: Vec<(String, String)> = got
        .iter()
        .map(|o| {
            let text = |key: &str| o[key].as_str().unwrap_or_default().to_owned();
            (text("target"), text("tx"))
        })
        .collect();
    let mut recorded = history_txs_for(&log, home, &p);
    reported.sort();
    recorded.sort();
    log.assert_eq(
        "history holds exactly the second applier's transactions",
        &recorded,
        &reported,
    );
    for id in ["retail", "classic"] {
        log.assert_eq(
            &format!("Foo on {id}"),
            &core(&race, id),
            &"-- Foo v2\n".to_owned(),
        );
    }
}

#[test]
fn race_interference_reports_committed_and_stale() {
    let log = tlog!();
    let race = race_home(&log, &["retail", "classic"]);
    let home = &race.home;
    log.step("seed and preview");
    let (p, _) = preview_both(&log, home);

    log.step("apply commits its first target");
    let pa = Pause::new(home.env.root());
    let a = cmd(&log, home, &["apply", &p])
        .failpoints(&format!("{CLEANUP}=pause"))
        .pause(&pa)
        .spawn();
    log.reached(&pa, CLEANUP);
    let (x, t1) = first_committed(&log, home, &p);
    let y = other(&x);
    let y_addons = home.addons(folder(&y));

    log.step("another command commits on the second target");
    let installed = cmd(
        &log,
        home,
        &[
            "install",
            "gh:o/bar",
            "--file",
            &race.bar_zip,
            "-t",
            &y,
            "--offline",
        ],
    )
    .run();
    installed.ok().assert_private();
    log.tree("Y addons", &y_addons);

    log.step("apply resumes");
    log.resume(&pa, CLEANUP);
    let a = a.wait();
    a.ok().assert_private();
    log.assert_eq("partial", &a.envelope()["partial"], &json!(true));
    let mut expected = vec![
        committed_on(&x, &t1),
        json!({"target": y, "status": "stale", "tx": null, "error_code": "plan_stale"}),
    ];
    expected.sort_by_key(|o| o["target"].as_str().unwrap_or_default().to_owned());
    log.assert_eq(
        "the first committed, the second stale",
        &outcomes(&log, &a),
        &expected,
    );
    let refused = a.data()["result"]["targets"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|t| t["target"]["id"] == y.as_str())
        .map(|t| t["error"]["code"].clone())
        .unwrap_or(Value::Null);
    log.assert_eq(
        "the update's own result refuses the second target",
        &refused,
        &json!("plan_stale"),
    );
    log.assert_eq(
        "the refusal mirrored as a warning",
        &a.warnings(),
        &vec!["plan_stale".to_owned()],
    );

    log.step("the second target is as the other command left it");
    log.assert_unchanged("Y addons", &y_addons);
    log.assert_eq(
        &format!("Foo on {y}"),
        &core(&race, &y),
        &"-- Foo v1\n".to_owned(),
    );
    log.assert_eq(
        &format!("Bar on {y}"),
        &y_addons.join("Bar/Bar.toc").exists(),
        &true,
    );
    log.assert_eq(
        &format!("Foo on {x}"),
        &core(&race, &x),
        &"-- Foo v2\n".to_owned(),
    );

    log.step("plans show");
    let shown = show(&log, home, &[&p], &[]);
    shown.ok();
    log.json("plans show", shown.data());
    log.assert_eq("state", &shown.data()["state"], &json!("partial"));
    log.assert_eq(
        "evidence",
        &evidence_txs(&shown),
        &vec![(x.clone(), t1.clone())],
    );
    log.assert_eq(
        "complete evidence",
        &shown.data()["evidence"]["complete"],
        &json!(true),
    );

    log.step("a second apply");
    let x_addons = home.addons(folder(&x));
    log.tree("X addons", &x_addons);
    let again = cmd(&log, home, &["apply", &p]).run();
    already_applied(&log, &again, &[t1.as_str()]);
    log.assert_unchanged("X addons", &x_addons);
    log.assert_unchanged("Y addons", &y_addons);
}

#[test]
fn lease_timeout_is_locked_with_the_holder() {
    let log = tlog!();
    let race = race_home(&log, &["retail", "classic"]);
    let home = &race.home;
    log.step("seed and preview");
    let (p, _) = preview_both(&log, home);

    log.step("first applier pauses between targets");
    let pa = Pause::new(home.env.root());
    let a = cmd(&log, home, &["apply", &p])
        .failpoints(&format!("{CLEANUP}=pause"))
        .pause(&pa)
        .spawn();
    log.reached(&pa, CLEANUP);
    let holder = a.pid();

    log.step("second applier times out");
    let lines = history_lines(&race);
    let b = cmd(&log, home, &["apply", &p])
        .timeout(Duration::from_secs(90))
        .run();
    // Resume the holder before asserting, so a failure never leaves it paused.
    log.resume(&pa, CLEANUP);
    b.fails(5, "locked").assert_private();
    let error = &b.envelope()["error"];
    log.json("refusal", error);
    log.assert_eq("retryable", &error["retryable"], &json!(true));
    log.assert_eq(
        "elapsed_ms in [30000, 45000)",
        &(30_000..45_000).contains(&b.elapsed_ms),
        &true,
    );
    let details = &error["details"];
    log.assert_eq("lease", &details["lease"], &json!(p));
    log.assert_eq("holder pid", &details["holder"]["pid"], &json!(holder));
    log.assert_eq(
        "holder command",
        &details["holder"]["command"],
        &json!("apply"),
    );
    log.assert_eq(
        "holder age_s >= 30",
        &details["holder"]["age_s"]
            .as_i64()
            .is_some_and(|age| age >= 30),
        &true,
    );
    log.assert_eq(
        "the fix reruns the apply",
        &error["fix_argv"],
        &json!(["omaddons", "apply", p]),
    );
    log.assert_eq("no history line added", &history_lines(&race), &lines);

    log.step("first applier finishes");
    let a = a.wait();
    a.ok().assert_private();
    let statuses: Vec<Value> = outcomes(&log, &a)
        .iter()
        .map(|o| o["status"].clone())
        .collect();
    log.assert_eq(
        "both committed",
        &statuses,
        &vec![json!("committed"), json!("committed")],
    );
    let txs: Vec<String> = history_txs_for(&log, home, &p)
        .into_iter()
        .map(|(_, tx)| tx)
        .collect();
    log.assert_eq("two transactions", &txs.len(), &2);

    log.step("the fix runs");
    let fix: Vec<String> = error["fix_argv"]
        .as_array()
        .into_iter()
        .flatten()
        .skip(1)
        .filter_map(|word| word.as_str().map(str::to_owned))
        .collect();
    let fix: Vec<&str> = fix.iter().map(String::as_str).collect();
    let again = cmd(&log, home, &fix).run();
    already_applied(&log, &again, &[txs[0].as_str(), txs[1].as_str()]);
}

#[test]
fn lease_held_by_an_applier_survives_the_sweeps() {
    let log = tlog!();
    let race = race_home(&log, &["retail"]);
    let home = &race.home;

    log.step("seed and preview");
    let previewed = cmd(&log, home, &["update", "foo", "-t", "retail", "--dry-run"]).run();
    previewed.ok().assert_private();
    let p = plan_id(&previewed);
    let plan = read_plan(&home.env, &p);
    log.json("plan file", &plan);
    let expires: Timestamp = plan["expires_at"].as_str().unwrap().parse().unwrap();
    let later = expires
        .checked_add(SignedDuration::from_hours(1))
        .unwrap()
        .to_string();
    log.note(format!(
        "the sweeping runs' clock: {later}, an hour past P's expiry"
    ));
    let plan_file = plans_dir(&home.env).join(format!("{p}.json"));
    let lease = lease_path(&home.env, &p);
    let names = |files: Vec<std::path::PathBuf>| -> Vec<String> {
        files
            .iter()
            .filter_map(|path| path.file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .collect()
    };
    let store = |label: &str| {
        log.json(
            &format!("plan files {label}"),
            &names(plan_files(&home.env)),
        );
        log.json(
            &format!("lease files {label}"),
            &names(lease_files(&home.env)),
        );
    };
    let present = |label: &str| {
        log.assert_eq(
            &format!("P's plan and lease {label}"),
            &(
                plan_files(&home.env).contains(&plan_file),
                lease_files(&home.env).contains(&lease),
            ),
            &(true, true),
        );
    };

    log.step("an applier holds the lease");
    let pa = Pause::new(home.env.root());
    let a = cmd(&log, home, &["apply", &p])
        .failpoints(&format!("{PLANNED}=pause"))
        .pause(&pa)
        .spawn();
    log.reached(&pa, PLANNED);
    log.assert_eq(
        "the applier holds the lease file open",
        &holds_open(a.pid(), &lease),
        &true,
    );

    log.step("the admission sweep past expiry");
    store("before the admission sweep");
    let dry_install = [
        "install",
        "gh:o/bar",
        "--file",
        race.bar_zip.as_str(),
        "-t",
        "retail",
        "--offline",
        "--dry-run",
    ];
    let stored = cmd(&log, home, &dry_install)
        .env("OMADDONS_NOW", &later)
        .run();
    stored.ok().assert_private();
    let q = plan_id(&stored);
    store("after the admission sweep");
    present("after the admission sweep");

    log.step("retention's sweep past expiry");
    let committed = cmd(
        &log,
        home,
        &[
            "install",
            "gh:o/bar",
            "--file",
            &race.bar_zip,
            "-t",
            "classic",
            "--offline",
        ],
    )
    .env("OMADDONS_NOW", &later)
    .run();
    committed.ok().assert_private();
    store("after retention's sweep");
    present("after retention's sweep");

    log.step("the applier finishes");
    log.resume(&pa, PLANNED);
    let a = a.wait();
    a.ok().assert_private();
    let got = outcomes(&log, &a);
    let statuses: Vec<(Value, Value)> = got
        .iter()
        .map(|o| (o["target"].clone(), o["status"].clone()))
        .collect();
    log.assert_eq(
        "retail committed",
        &statuses,
        &vec![(json!("retail"), json!("committed"))],
    );
    log.assert_eq(
        "Foo on retail",
        &core(&race, "retail"),
        &"-- Foo v2\n".to_owned(),
    );

    log.step("the next sweep takes them");
    cmd(&log, home, &dry_install)
        .env("OMADDONS_NOW", &later)
        .run()
        .ok()
        .assert_private();
    store("after the next admission sweep");
    log.assert_eq(
        "P's plan and lease gone",
        &(
            plan_files(&home.env).contains(&plan_file),
            lease_files(&home.env).contains(&lease),
        ),
        &(false, false),
    );
    log.assert_eq(
        "Q still there, unexpired at that clock",
        &plans_dir(&home.env).join(format!("{q}.json")).exists(),
        &true,
    );
}
