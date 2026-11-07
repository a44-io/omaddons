//! e2e suite: the `--if-match` binding's adversarial matrix, D2's design gate.
//!
//! Can a bound `install`, `update`, `sync` or `copy` ever commit something other than
//! its preview, or leave a client half-changed, when the world moves at the worst
//! moment? Each row runs the real binary on a fresh two-client home (retail, which the
//! run changes, and classic) through the engine's own failpoints:
//!
//! - M1: another commit on retail at every pause point before the lock is `plan_stale`
//!   (`targets`), and retail is as that commit left it;
//! - M2: a crash at every engine step after the check, then three lock-taking commands
//!   on classic: retail is wholly as before or wholly as previewed after each, and the
//!   bound run again commits exactly the preview from before, or is stale from after;
//! - M3: an injected I/O error at the swap leaves retail whole, and the same re-run rule;
//! - M4: two runs holding one token never both commit;
//! - M5: a stale bound run with nothing to do recovers nothing;
//! - M6: a folder the run cannot read stales it until it can;
//! - M7: a bound dry run holds no lock.
//!
//! A row whose failpoint the run never reaches fails: no row passes vacuously.

#![cfg(debug_assertions)]

use crate::common;

use std::path::PathBuf;

use common::binding as b;
use common::home::Home;
use common::pause::Pause;
use common::tlog::{Ran, TestLog};
use serde_json::{Value, json};

/// The bound commands, each changing retail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Command {
    Install,
    Update,
    Sync,
    Copy,
}

const COMMANDS: [Command; 4] = [
    Command::Install,
    Command::Update,
    Command::Sync,
    Command::Copy,
];

impl Command {
    const fn name(self) -> &'static str {
        match self {
            Self::Install => "install",
            Self::Update => "update",
            Self::Sync => "sync",
            Self::Copy => "copy",
        }
    }
}

/// One command on a fresh retail-and-classic home, with Bar and Baz published for both
/// clients as current releases nothing installs yet.
struct Shape {
    home: Home,
    args: Vec<String>,
}

#[cfg(test)]
impl Shape {
    /// - `install`: Foo v2 over v1 from `--file` archives (one folder replaced, one
    ///   removed), offline;
    /// - `update`: Foo v1 from `gh:o/foo`, v2 published (the same two folders), the memo
    ///   aged so the preview sees v2;
    /// - `sync`: an `addons.toml` that installs Foo (retail registered by Baz);
    /// - `copy`: Foo from classic to retail (retail registered by Baz).
    fn new(log: &TestLog, command: Command) -> Self {
        let home = Home::new(&["wow", "wow_classic"]);
        log.env(&home);
        let mm = b::MAINLINE_AND_MISTS;
        b::publish(&home, "bar", "Bar", "v1", mm);
        b::publish(&home, "baz", "Baz", "v1", mm);
        let install = |r: &str, client: &str| {
            log.run(&home, &["install", r, "-t", client, "--json"]).ok();
        };
        let args = match command {
            Command::Install => {
                let dl = home.env.root().join("dl");
                std::fs::create_dir_all(&dl).unwrap();
                for (name, entries) in [("v1", common::engine::V1), ("v2", common::engine::V2)] {
                    std::fs::write(
                        dl.join(format!("{name}.zip")),
                        common::archives::zip_text(entries),
                    )
                    .unwrap();
                }
                let zip = |name: &str| dl.join(format!("{name}.zip")).display().to_string();
                log.run(
                    &home,
                    &[
                        "install",
                        "gh:o/foo",
                        "--file",
                        &zip("v1"),
                        "-t",
                        "retail",
                        "--offline",
                        "--json",
                    ],
                )
                .ok();
                b::argv(&[
                    "install",
                    "gh:o/foo",
                    "--file",
                    &zip("v2"),
                    "-t",
                    "retail",
                    "--offline",
                ])
            }
            Command::Update => {
                release(&home, "v1", common::engine::V1);
                install("gh:o/foo", "retail");
                release(&home, "v2", common::engine::V2);
                b::age_release_memo(&home, jiff::SignedDuration::from_hours(1));
                b::argv(&["update", "Foo", "-t", "retail"])
            }
            Command::Sync => {
                b::publish(&home, "foo", "Foo", "v1", mm);
                install("gh:o/baz", "retail");
                home.write_desired("[family.mainline]\naddons = [\"gh:o/baz\", \"gh:o/foo\"]\n");
                b::argv(&["sync"])
            }
            Command::Copy => {
                b::publish(&home, "foo", "Foo", "v1", mm);
                install("gh:o/foo", "classic");
                install("gh:o/baz", "retail");
                b::argv(&["copy", "--from", "classic", "--to", "retail", "Foo"])
            }
        };
        Self { home, args }
    }

    fn args(&self) -> Vec<&str> {
        self.args.iter().map(String::as_str).collect()
    }

    fn fix(&self) -> Vec<String> {
        b::preview_argv(&self.args())
    }

    fn retail(&self) -> PathBuf {
        self.home.addons("_retail_")
    }

    fn journals(&self) -> Vec<String> {
        std::fs::read_dir(self.home.state().join("journal"))
            .map(|entries| {
                entries
                    .flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// Release `tag` of `gh:o/foo` as its latest, holding `entries` (retail only).
fn release(home: &Home, tag: &str, entries: &[(&str, &str)]) {
    let assets: [common::http::Asset<'_>; 1] =
        [("Foo.zip", &[("mainline", 120_105)], entries.to_vec())];
    common::http::gh_release(&home.http, "https://github.com/o/foo", tag, &assets);
}

/// Retail as a row compares it: its AddOns tree, its records without what every commit
/// rewrites (`tx_id`, `installed_at`) and their generation.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
struct View {
    addons: Option<String>,
    records: Value,
    generation: u64,
}

#[cfg(test)]
fn view(home: &Home) -> View {
    let addons = home.addons("_retail_");
    let record = common::state::record(home, "retail");
    let mut records = serde_json::to_value(&record.addons).unwrap();
    for addon in records
        .as_object_mut()
        .into_iter()
        .flat_map(|m| m.values_mut())
    {
        if let Some(map) = addon.as_object_mut() {
            map.remove("tx_id");
            map.remove("installed_at");
        }
    }
    View {
        addons: addons.is_dir().then(|| common::trees::tree_hash(&addons)),
        records,
        generation: record.generation,
    }
}

/// Which of `before` and `after` retail is now, logged; a third state fails the row.
#[track_caller]
fn verdict(log: &TestLog, home: &Home, before: &View, after: &View) -> &'static str {
    let now = view(home);
    // ubs:ignore — comparing client views and verdict names, not secrets
    let state = if now == *before {
        "before"
    // ubs:ignore — comparing client views, not secrets
    } else if now == *after {
        "after"
    } else {
        "neither"
    };
    log.json(
        "verdict",
        &json!({"state": state, "generation": now.generation}),
    );
    log.assert_eq(
        "retail is wholly before or wholly after",
        // ubs:ignore — comparing verdict names, not secrets
        &(state != "neither"),
        &true,
    );
    state
}

/// Retail after `command`'s bound run on an identical home with no failpoint: what the
/// preview approved (a token never crosses homes: their target keys differ).
fn expected_after(log: &TestLog, command: Command) -> View {
    log.step(&format!(
        "{}: the expected result, on an identical home",
        command.name()
    ));
    let shape = Shape::new(log, command);
    let (_, token) = b::preview(log, &shape.home, command.name(), &shape.args());
    log.run(&shape.home, &b::bound(&shape.args(), &token)).ok();
    let after = view(&shape.home);
    log.json("after", &after);
    after
}

/// The lock-taking commands on classic that recover a journal left on retail.
const RECOVERERS: [[&str; 6]; 3] = [
    [
        "install",
        "gh:o/bar",
        "-t",
        "classic",
        "--json",
        "--offline",
    ],
    ["pin", "Bar", "-t", "classic", "--json", "--offline"],
    ["unpin", "Bar", "-t", "classic", "--json", "--offline"],
];

/// Recovers with the three commands of [`RECOVERERS`] in turn: after each, retail is
/// wholly before or after; after the first no journal remains, and the others change
/// nothing more. Returns the verdict.
fn recover_three_times(log: &TestLog, shape: &Shape, before: &View, after: &View) -> &'static str {
    let mut settled: Option<View> = None;
    for (i, recoverer) in RECOVERERS.iter().enumerate() {
        log.note(format!("recovery {} of 3: {}", i + 1, recoverer[0]));
        // The first install of Bar needs its release; the others are records only.
        let argv: Vec<&str> = if i == 0 {
            recoverer
                .iter()
                .copied()
                .filter(|a| *a != "--offline")
                .collect()
        } else {
            recoverer.to_vec()
        };
        log.run(&shape.home, &argv).ok();
        verdict(log, &shape.home, before, after);
        log.assert_eq(
            "no journal remains",
            &shape.journals(),
            &Vec::<String>::new(),
        );
        let now = view(&shape.home);
        match &settled {
            None => settled = Some(now),
            Some(first) => log.assert_eq("a later recovery changes nothing on retail", &now, first),
        }
    }
    verdict(log, &shape.home, before, after)
}

/// The re-run rule: the bound run again with its token commits exactly the preview from
/// `before`, and is stale from `after`: its generation moved (`targets`), and with the
/// work done it now plans nothing, so `selection` and `observed` differ too.
fn rerun(log: &TestLog, shape: &Shape, token: &str, state: &str, after: &View) {
    log.step(&format!("the re-run, from {state}"));
    let now = view(&shape.home);
    let ran = log.run(&shape.home, &b::bound(&shape.args(), token));
    // ubs:ignore — comparing verdict names, not secrets
    if state == "before" {
        ran.ok();
        log.assert_eq("retail is as previewed", &view(&shape.home), after);
    } else {
        b::assert_stale_with(log, &ran, "targets", &shape.fix());
        log.assert_eq("retail unchanged", &view(&shape.home), &now);
    }
}

/// The child's run, which must have stopped at its failpoint: an `abort` kills it with
/// `SIGABRT`, so exit 0 (or any envelope) means the point was never reached.
#[track_caller]
fn crashed(log: &TestLog, ran: &Ran, point: &str) {
    log.json(
        "child",
        &json!({"failpoint": point, "exit": ran.run.code, "signal": ran.signal}),
    );
    if ran.signal.is_none() {
        log.assert_eq(&format!("{point} reached"), &false, &true);
    }
    ran.aborted();
}

// ---- M1 ------------------------------------------------------------------------------

/// The pause points before the lock, in run order. (`engine.plan.downloaded` is reached
/// only by a run that downloads; every bound run here finds its archive in the blob
/// cache its preview filled, so none does.)
const PRE_LOCK: [&str; 5] = [
    "engine.plan.fetched",
    "engine.plan.done",
    "engine.stage.begin",
    "engine.stage.synced",
    "engine.snapshot.prestaged",
];

#[test]
fn a_commit_at_each_pre_lock_pause_stales_the_bound_run() {
    let log = common::tlog::tlog!();
    for command in COMMANDS {
        for point in PRE_LOCK {
            log.step(&format!("{} × {point}", command.name()));
            let shape = Shape::new(&log, command);
            let (_, token) = b::preview(&log, &shape.home, command.name(), &shape.args());
            log.tree("retail", &shape.retail());
            let history = common::state::history(&shape.home).len();

            let pause = Pause::new(shape.home.env.root());
            let child = log
                .cmd(&shape.home, &b::bound(&shape.args(), &token))
                .failpoints(&format!("{point}=pause"))
                .pause(&pause)
                .spawn();
            log.reached(&pause, point);
            log.note("another install commits on retail while the bound run waits");
            log.run(
                &shape.home,
                &["install", "gh:o/bar", "-t", "retail", "--json"],
            )
            .ok();
            log.tree("after-concurrent", &shape.retail());
            let concurrent = view(&shape.home);
            log.resume(&pause, point);
            let ran = child.wait();

            b::assert_stale(&log, &ran, &["targets"], &shape.fix());
            log.assert_unchanged("after-concurrent", &shape.retail());
            log.assert_eq(
                "retail as the concurrent commit left it",
                &view(&shape.home),
                &concurrent,
            );
            log.assert_eq(
                "history gained Bar's line alone",
                &(common::state::history(&shape.home).len() - history),
                &1,
            );
            log.assert_eq(
                "no journal remains",
                &shape.journals(),
                &Vec::<String>::new(),
            );
        }
    }
}

// ---- M2 ------------------------------------------------------------------------------

/// The engine's failpoints after the token check through the commit, in run order, that
/// a bound run on an installed client reaches. (`engine.addons_dir.created` fires only on
/// a client's first install; journaled renames are crashed one by one with `@N`.)
const AFTER_CHECK: [&str; 16] = [
    "engine.stage.begin",
    "engine.stage.file",
    "engine.stage.synced",
    "engine.snapshot.prestaged",
    "engine.lock.acquired",
    "engine.recovered",
    "engine.registered",
    "engine.revalidated",
    "engine.journal.prepared",
    "engine.snapshot.sv",
    "engine.snapshot.state",
    "engine.swap.after_exchange",
    "engine.swapped",
    "engine.record.written",
    "engine.history.appended",
    "engine.committed",
];

/// A failpoint and, for one that fires more than once, the hit to act on (`@N`).
type Point<'a> = (&'a str, Option<usize>);

fn spec((name, at): Point<'_>, action: &str) -> String {
    match at {
        Some(n) => format!("{name}={action}@{n}"),
        None => format!("{name}={action}"),
    }
}

/// One M2 row: a crash at `point`, recovery three times, the re-run rule.
fn crash_row(log: &TestLog, command: Command, point: Point<'_>, after: &View) {
    let armed = spec(point, "abort");
    log.step(&format!("{} × {armed}", command.name()));
    let shape = Shape::new(log, command);
    let (_, token) = b::preview(log, &shape.home, command.name(), &shape.args());
    let before = view(&shape.home);
    log.json("before", &before);
    let ran = log
        .cmd(&shape.home, &b::bound(&shape.args(), &token))
        .failpoints(&armed)
        .run();
    crashed(log, &ran, &armed);
    let state = recover_three_times(log, &shape, &before, after);
    rerun(log, &shape, &token, state, after);
}

/// The journaled renames of the preview: a folder added, replaced or removed is one each.
fn renames(dry: &Ran) -> usize {
    let items = match dry.data().get("items") {
        Some(items) => items.clone(),
        None => b::items(dry.data()),
    };
    items
        .as_array()
        .into_iter()
        .flatten()
        .map(|i| {
            ["added", "replaced", "removed"]
                .iter()
                .map(|k| i["folders"][k].as_array().map_or(0, Vec::len))
                .sum::<usize>()
        })
        .sum()
}

#[test]
fn a_bound_run_crashed_after_its_check_recovers_whole() {
    let log = common::tlog::tlog!();
    for command in [Command::Install, Command::Update] {
        let after = expected_after(&log, command);
        // How many renames the swap journals: each one is crashed before it runs.
        let probe = Shape::new(&log, command);
        let (dry, _) = b::preview(&log, &probe.home, command.name(), &probe.args());
        let n = renames(&dry);
        log.json("journaled renames", &n);
        log.assert_eq("a replace and a remove at least", &(n >= 2), &true);
        let points = AFTER_CHECK
            .iter()
            .map(|p| (*p, None))
            .chain((1..=n).map(|i| ("engine.swap.before_rename", Some(i))));
        for point in points {
            crash_row(&log, command, point, &after);
        }
    }
    for command in [Command::Sync, Command::Copy] {
        let after = expected_after(&log, command);
        crash_row(&log, command, ("engine.swapped", None), &after);
    }
}

// ---- M3 ------------------------------------------------------------------------------

/// The swap's failpoints, in run order, and whether an injected I/O error there fails
/// the run. Before the journal says `swapped` the error surfaces and the engine rolls the
/// transaction back; from it on, the engine rolls it forward in process and the
/// transaction stands, so the command reports it (the engine's own rule, pinned by
/// `tx::tests::an_error_at_any_failpoint_leaves_the_pin_iff_its_history_line`).
const SWAP: [(Point<'static>, bool); 4] = [
    (("engine.swap.before_rename", Some(1)), true),
    (("engine.swap.after_exchange", None), true),
    (("engine.swapped", None), false),
    (("engine.record.written", None), false),
];

#[test]
fn an_injected_io_error_at_the_swap_leaves_the_client_whole() {
    let log = common::tlog::tlog!();
    for command in [Command::Install, Command::Update] {
        let after = expected_after(&log, command);
        for (point, fails) in SWAP {
            let armed = spec(point, "error");
            log.step(&format!("{} × {armed}", command.name()));
            let shape = Shape::new(&log, command);
            let (_, token) = b::preview(&log, &shape.home, command.name(), &shape.args());
            let before = view(&shape.home);
            let ran = log
                .cmd(&shape.home, &b::bound(&shape.args(), &token))
                .failpoints(&armed)
                .run();
            let state = if fails {
                ran.exit(1);
                log.assert_eq(
                    &format!("{armed} reached: the injected error, not a refusal"),
                    // ubs:ignore — comparing error codes, not secrets
                    &ran.error_code().is_some_and(|c| c != "plan_stale"),
                    &true,
                );
                "before"
            } else {
                // Rolled forward in process: the run commits whether or not it reached
                // the point, so the same run on an identical home, armed to abort
                // there, proves it does.
                ran.ok();
                let sibling = Shape::new(&log, command);
                let (_, own) = b::preview(&log, &sibling.home, command.name(), &sibling.args());
                let aborted = spec(point, "abort");
                let probe = log
                    .cmd(&sibling.home, &b::bound(&sibling.args(), &own))
                    .failpoints(&aborted)
                    .run();
                crashed(&log, &probe, &aborted);
                "after"
            };
            log.assert_eq(
                "retail, wholly",
                &verdict(&log, &shape.home, &before, &after),
                &state,
            );
            let state = recover_three_times(&log, &shape, &before, &after);
            rerun(&log, &shape, &token, state, &after);
        }
    }
}

// ---- M4 ------------------------------------------------------------------------------

/// The `update` lines of history, for "exactly one committed".
fn updates(home: &Home) -> usize {
    common::state::history(home)
        .iter()
        .filter(|e| serde_json::to_value(e.kind).ok() == Some(json!("update")))
        .count()
}

#[test]
fn two_concurrent_bound_runs_never_both_commit() {
    let log = common::tlog::tlog!();
    let after = expected_after(&log, Command::Update);
    log.step("update: two runs holding one token, both stopped before the lock");
    let shape = Shape::new(&log, Command::Update);
    let (_, token) = b::preview(&log, &shape.home, "update", &shape.args());
    let committed = updates(&shape.home);
    let run = b::bound(&shape.args(), &token);
    let point = "engine.snapshot.prestaged";
    let (first, second) = (
        Pause::new(shape.home.env.root()),
        Pause::new(shape.home.env.root()),
    );
    let spawn = |pause: &Pause| {
        log.cmd(&shape.home, &run)
            .failpoints(&format!("{point}=pause"))
            .pause(pause)
            .spawn()
    };
    let (a, b_run) = (spawn(&first), spawn(&second));
    log.reached(&first, point);
    log.reached(&second, point);

    log.step("the first goes on and commits");
    log.resume(&first, point);
    a.wait().ok();
    log.step("the second goes on: its client moved under it");
    log.resume(&second, point);
    let ran = b_run.wait();
    b::assert_stale(&log, &ran, &["targets"], &shape.fix());
    log.assert_eq("retail is as previewed, once", &view(&shape.home), &after);
    log.assert_eq(
        "exactly one update committed",
        &(updates(&shape.home) - committed),
        &1,
    );
}

#[test]
fn a_bound_run_that_plans_after_the_winner_is_stale_not_unchanged() {
    let log = common::tlog::tlog!();
    for command in [Command::Update, Command::Copy] {
        log.step(&format!("{}: the first run with the token", command.name()));
        let shape = Shape::new(&log, command);
        let (_, token) = b::preview(&log, &shape.home, command.name(), &shape.args());
        log.run(&shape.home, &b::bound(&shape.args(), &token)).ok();
        let now = view(&shape.home);
        log.step(&format!(
            "{}: the second, with nothing left to do",
            command.name()
        ));
        let ran = log.run(&shape.home, &b::bound(&shape.args(), &token));
        b::assert_stale_with(&log, &ran, "targets", &shape.fix());
        b::assert_stale_with(&log, &ran, "selection", &shape.fix());
        log.assert_eq("retail unchanged", &view(&shape.home), &now);
    }
}

// ---- M5 ------------------------------------------------------------------------------

#[test]
fn a_stale_bound_noop_run_recovers_nothing() {
    let log = common::tlog::tlog!();
    let home = Home::new(&["wow", "wow_classic"]);
    log.env(&home);
    log.step("setup: Foo current and Bar on retail");
    for (repo, name) in [("foo", "Foo"), ("bar", "Bar")] {
        b::publish(&home, repo, name, "v1", b::MAINLINE_AND_MISTS);
        log.run(
            &home,
            &["install", &format!("gh:o/{repo}"), "-t", "retail", "--json"],
        )
        .ok();
    }
    let update = ["update", "Foo", "-t", "retail"];
    log.step("preview: nothing to do");
    let (_, token) = b::preview(&log, &home, "update", &update);
    log.step("Bar pinned (the token is stale), then an install crashed after its swap");
    log.run(&home, &["pin", "Bar", "-t", "retail", "--json"])
        .ok();
    let baz = home.env.root().join("baz.zip");
    std::fs::write(
        &baz,
        common::archives::zip_text(&[("Baz/Baz.toc", "## Interface: 120105\n## Version: 1\n")]),
    )
    .unwrap();
    let crash = [
        "install",
        "gh:o/baz",
        "--file",
        baz.to_str().unwrap(),
        "-t",
        "retail",
        "--offline",
        "--json",
    ];
    let ran = log
        .cmd(&home, &crash)
        .failpoints("engine.swapped=abort")
        .run();
    crashed(&log, &ran, "engine.swapped=abort");
    let journals = || -> Vec<String> {
        std::fs::read_dir(home.state().join("journal"))
            .map(|e| {
                e.flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default()
    };
    let pending = journals();
    log.json("journals", &pending);
    log.assert_eq("a journal waits", &pending.is_empty(), &false);
    let guard = b::guard(&log, &home, "pending");

    log.step("the stale bound run: refused before any recovery");
    let ran = log.run(&home, &b::bound(&update, &token));
    b::assert_stale_with(&log, &ran, "targets", &b::preview_argv(&update));
    log.assert_eq("the journal still waits", &journals(), &pending);
    guard.unchanged(&log);

    log.step("an unbound lock-taking command recovers it, as before");
    log.run(&home, &["unpin", "Bar", "-t", "retail", "--json"])
        .ok();
    log.assert_eq("recovered", &journals(), &Vec::<String>::new());
}

// ---- M6 ------------------------------------------------------------------------------

#[test]
fn an_unreadable_replaced_folder_stales_the_bound_run() {
    use std::os::unix::fs::PermissionsExt;
    const TEST: &str = "an_unreadable_replaced_folder_stales_the_bound_run";
    if rustix::process::geteuid().is_root() {
        common::checkpoint::skip(TEST, "running as root, where mode 000 does not deny reads");
        return;
    }
    let log = common::tlog::tlog!();
    let after = expected_after(&log, Command::Update);
    log.step("update: preview");
    let shape = Shape::new(&log, Command::Update);
    let (_, token) = b::preview(&log, &shape.home, "update", &shape.args());
    let before = view(&shape.home);
    let core = shape.retail().join("Foo/core.lua");
    let mode = std::fs::metadata(&core).unwrap().permissions().mode();

    log.step("Foo/core.lua made unreadable");
    std::fs::set_permissions(&core, std::fs::Permissions::from_mode(0o000)).unwrap();
    let ran = log.run(&shape.home, &b::bound(&shape.args(), &token));
    b::assert_stale_with(&log, &ran, "selection", &shape.fix());
    // A fresh preview: Foo, its only item, fails with `io` (it cannot be witnessed), so
    // the run is that error, the item in `details.items`.
    let mut dry = shape.args();
    dry.extend(["--dry-run", "--json"]);
    let fresh = log.run(&shape.home, &dry);
    let error = &fresh.fails(1, "io").envelope()["error"];
    let item = &error["details"]["items"][0]["error"];
    log.assert_eq(
        "a fresh preview: (Foo's error, it names the unreadable file)",
        &(
            &item["code"],
            item["message"]
                .as_str()
                .is_some_and(|m| m.contains("core.lua") && m.contains("cannot be witnessed")),
        ),
        &(&json!("io"), true),
    );

    log.step("readable again: the world is as previewed, so the token binds");
    std::fs::set_permissions(&core, std::fs::Permissions::from_mode(mode)).unwrap();
    log.assert_eq("Foo unchanged", &view(&shape.home), &before);
    log.run(&shape.home, &b::bound(&shape.args(), &token)).ok();
    log.assert_eq("retail is as previewed", &view(&shape.home), &after);
}

// ---- M7 ------------------------------------------------------------------------------

#[test]
fn a_paused_bound_dry_run_holds_no_lock() {
    let log = common::tlog::tlog!();
    log.step("install: preview");
    let shape = Shape::new(&log, Command::Install);
    let (_, token) = b::preview(&log, &shape.home, "install", &shape.args());
    let mut dry = shape.args();
    dry.push("--dry-run");

    log.step("the bound dry run stops after planning");
    let pause = Pause::new(shape.home.env.root());
    let point = "engine.plan.done";
    let child = log
        .cmd(&shape.home, &b::bound(&dry, &token))
        .failpoints(&format!("{point}=pause"))
        .pause(&pause)
        .spawn();
    log.reached(&pause, point);
    log.step("another install takes the lock and commits meanwhile");
    log.note("it would wait for the lock, and fail `locked` after 5 s, were it held");
    log.cmd(
        &shape.home,
        &["install", "gh:o/bar", "-t", "retail", "--json"],
    )
    .timeout(std::time::Duration::from_secs(10))
    .run()
    .ok();
    let state = b::state_without_net_log(&log, &shape.home, "after the concurrent install");

    log.step("the dry run goes on, and writes nothing");
    log.resume(&pause, point);
    child.wait();
    log.assert_eq(
        "<state> as the concurrent install left it",
        &b::state_without_net_log(&log, &shape.home, "after the dry run"),
        &state,
    );
}
