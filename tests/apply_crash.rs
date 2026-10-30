//! e2e suite: apply under crashes, recovery and I/O failures: an applying process crashed at every commit failpoint and recovered (once, then twice more), recovery itself crashed, a killed lease holder, the reload after waiting for the lease, injected I/O failures before anything runs, and a lost or failed ending, each leaving the plan applied exactly once or not at all (D5's design gate).
#![cfg(test)]
#![cfg(debug_assertions)]

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use common::home::Home;
use common::pause::Pause;
use common::plans::{plans_dir, read_plan, wait_until_open};
use common::tlog::{Cmd, Ran, TestLog, tlog};
use common::trees::tree_hash;
use omaddons::fsx::{Lease, ScopeTable};
use omaddons::model::PlanId;

// ---------------------------------------------------------------------------------------
// Support.

/// Foo: two folders, the second requiring the first, both loaded by retail.
const FOO: [(&str, &str); 3] = [
    (
        "Foo/Foo.toc",
        "## Interface: 120105\n## Version: 1\nFoo.lua\n",
    ),
    ("Foo/Foo.lua", "local foo = true\n"),
    (
        "Foo_Config/Foo_Config.toc",
        "## Interface: 120105\n## Version: 1\n## Dependencies: Foo\n",
    ),
];
const FOLDERS: [&str; 2] = ["Foo", "Foo_Config"];
/// Bar, installed on Mists Classic by the commands that recover.
const BAR: [(&str, &str); 1] = [(
    "Bar/Bar.toc",
    "## Interface: 120105, 50504\n## Version: 1\n",
)];

fn strings(args: &[&str]) -> Vec<String> {
    args.iter().map(|a| (*a).to_owned()).collect()
}

/// Retail, registered before anything is previewed (a crash after registration then
/// changes no target identity), and Mists Classic; Foo's zip to install on retail and
/// Bar's on classic, both local, so nothing reaches the network.
struct Home2 {
    home: Home,
    foo: String,
    bar: String,
}

impl Home2 {
    fn new(log: &TestLog) -> Self {
        log.step("seed");
        let home = Home::new(&["wow", "wow_classic"]);
        log.env(&home);
        let zip = |name: &str, entries: &[(&str, &str)]| {
            let path = home.env.root().join(format!("{name}.zip"));
            fs::write(&path, common::archives::zip_text(entries)).unwrap();
            path.display().to_string()
        };
        let foo = zip("foo", &FOO);
        let bar = zip("bar", &BAR);
        let h = Self { home, foo, bar };
        h.ok(log, &["targets", "default", "retail"]);
        h
    }

    /// `omaddons <args> --json --progress json`.
    fn cmd<'a>(&self, log: &'a TestLog, args: &[&str]) -> Cmd<'a> {
        let mut argv = strings(args);
        argv.extend(strings(&["--json", "--progress", "json"]));
        log.cmd(&self.home, &argv)
    }

    fn ok(&self, log: &TestLog, args: &[&str]) -> Ran {
        let ran = self.cmd(log, args).run();
        ran.ok().assert_private();
        ran
    }

    /// Previews Foo's install on retail; returns the stored plan's id.
    fn plan_install_foo(&self, log: &TestLog) -> String {
        log.step("preview");
        let ran = self.ok(
            log,
            &[
                "install",
                "gh:o/foo",
                "--file",
                &self.foo,
                "-t",
                "retail",
                "--dry-run",
            ],
        );
        let id = ran.data()["plan_id"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        assert!(!id.is_empty(), "no plan stored: {}", ran.data());
        log.json("plan file", &read_plan(&self.home.env, &id));
        id
    }

    fn apply(&self, log: &TestLog, id: &str) -> Ran {
        self.cmd(log, &["apply", id]).run()
    }

    fn lease(&self, id: &str) -> PathBuf {
        plans_dir(&self.home.env).join(format!("{id}.lease"))
    }

    /// Takes the plan's lease from the test process.
    fn hold_lease(&self, id: &str) -> omaddons::fsx::LeaseGuard {
        let paths = common::http::paths(&self.home.env);
        let scopes = ScopeTable::new(&paths, &[], None).unwrap();
        let id: PlanId = id.parse().unwrap();
        Lease::acquire(
            &scopes,
            &paths.plan_lease(&id),
            "test",
            Duration::from_secs(5),
        )
        .map_err(|_| "the plan's lease")
        .unwrap()
    }

    /// Whether the plan's lease is free now (a non-blocking take that succeeds).
    fn lease_is_free(&self, id: &str) -> bool {
        let paths = common::http::paths(&self.home.env);
        let scopes = ScopeTable::new(&paths, &[], None).unwrap();
        let id: PlanId = id.parse().unwrap();
        matches!(
            Lease::try_acquire(&scopes, &paths.plan_lease(&id), "test"),
            Ok(Some(_))
        )
    }

    /// Every client dir, `<state>` and `<cache>/plans`, as `log.tree` records them.
    fn trees(&self) -> Vec<(&'static str, PathBuf)> {
        vec![
            ("retail", self.home.client("_retail_")),
            ("classic", self.home.client("_classic_")),
            ("state", self.home.state()),
            ("plans", plans_dir(&self.home.env)),
        ]
    }

    /// A reader's view after a crash: `status` names any unfinished transaction and
    /// changes nothing. Returns whether a journal waits.
    fn after_the_crash(&self, log: &TestLog) -> bool {
        log.step("after the crash");
        for (label, root) in self.trees() {
            log.tree(label, &root);
        }
        let status = self.cmd(log, &["status"]).run();
        status.ok();
        for (label, root) in self.trees() {
            log.assert_unchanged(label, &root);
        }
        status.warnings().iter().any(|w| w == "journal_pending")
    }

    /// A lock-taking commit on classic, which recovers every pending journal first and
    /// touches retail's records only through that recovery.
    fn recover(&self, log: &TestLog, args: &[&str], pending: bool) -> Ran {
        log.step("recover");
        let ran = self.ok(log, args);
        if pending {
            assert!(
                ran.warnings().iter().any(|w| w == "journal_recovered"),
                "{args:?} recovered nothing: {:?}",
                ran.warnings()
            );
        }
        ran
    }

    fn install_bar(&self) -> [&str; 6] {
        ["install", "gh:o/bar", "--file", &self.bar, "-t", "classic"]
    }
}

/// Foo's folders as a direct install on an identical home leaves them.
fn reference(log: &TestLog) -> BTreeMap<String, String> {
    let log = log.case("reference: a direct install");
    let twin = Home2::new(&log);
    twin.ok(
        &log,
        &["install", "gh:o/foo", "--file", &twin.foo, "-t", "retail"],
    );
    FOLDERS
        .iter()
        .map(|f| {
            (
                (*f).to_owned(),
                tree_hash(&twin.home.addons("_retail_").join(f)),
            )
        })
        .collect()
}

/// Whether the plan is on disk.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
enum Applied {
    None,
    /// With the tx its one history line names.
    Once(String),
}

/// What the plan left on retail: Foo's folders wholly absent or wholly the approved ones
/// (never mixed), Foo in the record exactly when they are there, exactly one history
/// line carrying the plan (with its `display_argv`) exactly then, and no journal left.
fn oracle(log: &TestLog, h: &Home2, id: &str, reference: &BTreeMap<String, String>) -> Applied {
    let addons = h.home.addons("_retail_");
    let folders: BTreeMap<String, Option<String>> = FOLDERS
        .iter()
        .map(|f| {
            let dir = addons.join(f);
            ((*f).to_owned(), dir.exists().then(|| tree_hash(&dir)))
        })
        .collect();
    let listed = h.ok(log, &["list", "-t", "retail"]);
    let managed = listed.data()["targets"][0]["addons"]
        .as_array()
        .is_some_and(|addons| addons.iter().any(|a| a["name"] == json!("Foo")));
    let history = fs::read_to_string(h.home.state().join("history.jsonl")).unwrap_or_default();
    let lines: Vec<Value> = history
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|line| line["plan"] == json!(id))
        .collect();
    let journals: Vec<String> = fs::read_dir(h.home.state().join("journal"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    log.json(
        "oracle",
        &json!({"folders": folders, "managed": managed, "plan_lines": lines, "journals": journals}),
    );
    let present = folders.values().all(Option::is_some);
    let absent = folders.values().all(Option::is_none);
    assert!(present || absent, "Foo's folders are mixed: {folders:?}");
    assert!(journals.is_empty(), "a journal is left: {journals:?}");
    log.assert_eq(
        "Foo is recorded exactly when its folders are",
        &managed,
        &present,
    );
    log.assert_eq(
        "lines carrying the plan",
        &lines.len(),
        &usize::from(present),
    );
    if !present {
        return Applied::None;
    }
    for (folder, hash) in &folders {
        log.assert_eq(
            &format!("{folder} as a direct install leaves it"),
            &hash.as_ref(),
            &reference.get(folder),
        );
    }
    let plan = read_plan(&h.home.env, id);
    log.assert_eq("the line's argv", &lines[0]["argv"], &plan["display_argv"]);
    Applied::Once(lines[0]["tx"].as_str().unwrap_or_default().to_owned())
}

/// The re-apply after `first`: `already_applied` naming its tx, trees unchanged; or,
/// when nothing was applied, a commit that applies it once.
fn reapply(
    log: &TestLog,
    h: &Home2,
    id: &str,
    first: &Applied,
    reference: &BTreeMap<String, String>,
) -> Applied {
    log.step("re-apply");
    match first {
        Applied::Once(tx) => {
            for (label, root) in h.trees() {
                log.tree(label, &root);
            }
            let ran = h.apply(log, id);
            ran.fails(1, "plan_stale").assert_private();
            let details = &ran.envelope()["error"]["details"];
            log.assert_eq("reason", &details["reason"], &json!("already_applied"));
            log.assert_eq("txs", &details["txs"], &json!([tx]));
            // The refusal took the plan's lease: the lease file (if new) is its only
            // trace, and the plan files, clients and state are as they were.
            for (label, root) in h.trees().into_iter().filter(|(l, _)| *l != "plans") {
                log.assert_unchanged(label, &root);
            }
            first.clone()
        }
        Applied::None => {
            let ran = h.apply(log, id);
            ran.ok().assert_private();
            log.assert_eq(
                "committed",
                &ran.data()["outcomes"][0]["status"],
                &json!("committed"),
            );
            let now = oracle(log, h, id, reference);
            assert!(matches!(now, Applied::Once(_)), "{now:?}");
            now
        }
    }
}

// ---------------------------------------------------------------------------------------
// The crash matrix of an applied commit.

/// What a crash at a failpoint leaves once recovered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expect {
    /// Rolled back, or never started.
    None,
    /// Rolled forward, or committed.
    Once,
    /// The install's commit never reaches this point: the run succeeds.
    NotReached,
}

/// Every failpoint of an install's commit window, with what recovery leaves.
fn commit_failpoints() -> Vec<(String, Expect)> {
    let mut points: Vec<(String, Expect)> = Vec::new();
    let mut before = true;
    for name in omaddons::engine::FAILPOINTS {
        if name.starts_with("engine.recover.") {
            break;
        }
        if *name == "engine.swapped" {
            before = false;
        }
        // A local zip downloads nothing, and retail's AddOns exists already.
        let expect = match *name {
            "engine.plan.downloaded" | "engine.addons_dir.created" => Expect::NotReached,
            _ if before => Expect::None,
            _ => Expect::Once,
        };
        if *name == "engine.swap.before_rename" {
            for n in 1..=FOLDERS.len() {
                points.push((format!("{name}=abort@{n}"), expect));
            }
        } else {
            points.push((format!("{name}=abort"), expect));
        }
    }
    points
}

/// One crash case: a fresh home, the plan, the apply crashed at `spec`, a reader, the
/// recovery (with `recover_spec` armed when given), then the oracle, the re-apply and two
/// more recoveries. Returns (whether the apply crashed, whether a journal was left,
/// whether the recovery crashed, the oracle).
fn crash_case(
    log: &TestLog,
    spec: &str,
    recover_spec: Option<&str>,
    reference: &BTreeMap<String, String>,
) -> (bool, bool, bool, Applied) {
    let h = Home2::new(log);
    let id = h.plan_install_foo(log);
    let addons = h.home.addons("_retail_");
    log.tree("addons-retail", &addons);

    log.step("crash");
    let ran = h.cmd(log, &["apply", &id]).failpoints(spec).run();
    let crashed = ran.signal.is_some();
    if crashed {
        ran.aborted();
    } else {
        ran.ok().assert_private();
    }
    assert!(h.lease(&id).exists(), "no lease file after a real apply");
    assert!(
        h.lease_is_free(&id),
        "the crashed applier's lease still blocks"
    );
    let pending = h.after_the_crash(log);

    let install_bar = h.install_bar();
    let mut recovery_crashed = false;
    if let Some(recover_spec) = recover_spec {
        log.step("recover, crashed");
        let ran = h.cmd(log, &install_bar).failpoints(recover_spec).run();
        recovery_crashed = ran.signal.is_some();
        if recovery_crashed {
            ran.aborted();
        } else {
            ran.ok();
        }
    }
    let still_pending = recover_spec.is_none() || h.after_the_crash(log);
    h.recover(log, &install_bar, pending && still_pending);
    let first = oracle(log, &h, &id, reference);
    if first == Applied::None {
        log.assert_unchanged("addons-retail", &addons);
    }
    let now = reapply(log, &h, &id, &first, reference);

    log.step("repeated recovery");
    for args in [
        ["pin", "Bar", "-t", "classic"],
        ["unpin", "Bar", "-t", "classic"],
    ] {
        h.ok(log, &args);
        log.assert_eq("still applied once", &oracle(log, &h, &id, reference), &now);
    }
    (crashed, pending, recovery_crashed, first)
}

#[test]
fn apply_crash_at_every_commit_failpoint_applies_once_or_not_at_all() {
    let log = tlog!();
    let reference = reference(&log);
    let mut wrong = Vec::new();
    for (spec, expect) in commit_failpoints() {
        let log = log.case(&spec);
        let (crashed, pending, _, first) = crash_case(&log, &spec, None, &reference);
        let actual = match (crashed, &first) {
            (false, _) => Expect::NotReached,
            (true, Applied::None) => Expect::None,
            (true, Applied::Once(_)) => Expect::Once,
        };
        log.json(
            "row",
            &json!({"spec": spec, "crashed": crashed, "pending": pending, "expected": format!("{expect:?}"), "actual": format!("{actual:?}")}),
        );
        if actual != expect {
            wrong.push(format!("{spec}: expected {expect:?}, got {actual:?}"));
        }
    }
    assert!(
        wrong.is_empty(),
        "the case table is wrong:\n{}",
        wrong.join("\n")
    );
}

// ---------------------------------------------------------------------------------------
// Recovery crashed, under apply.

#[test]
fn apply_recovery_crashed_keeps_the_plan_exactly_once() {
    let log = tlog!();
    let reference = reference(&log);
    let mut crashed: Vec<String> = Vec::new();
    for (spec, _) in commit_failpoints() {
        // Only a crash that leaves a journal has a recovery to crash.
        let probe = log.case(&format!("{spec}: probe"));
        let (_, pending, _, _) = crash_case(&probe, &spec, None, &reference);
        if !pending {
            continue;
        }
        // Each rollback step until one is not reached (nor are the steps after it), then
        // the roll-forward's start.
        let steps = (1..=8).map(|k| format!("engine.recover.step=abort@{k}"));
        let mut rolled_back = true;
        for recover in steps.chain(["engine.recover.forward=abort".to_owned()]) {
            let forward = recover.starts_with("engine.recover.forward");
            if !forward && !rolled_back {
                continue;
            }
            let log = log.case(&format!("{spec}, {recover}"));
            let (_, _, recovery_crashed, first) =
                crash_case(&log, &spec, Some(&recover), &reference);
            log.json(
                "recovery",
                &json!({"crashed": recovery_crashed, "oracle": format!("{first:?}")}),
            );
            if recovery_crashed {
                crashed.push(format!("{spec}, {recover}"));
            } else if !forward {
                rolled_back = false;
            }
        }
    }
    log.json("recoveries crashed", &crashed);
    assert!(
        crashed.iter().any(|c| c.contains("engine.recover.forward")),
        "no roll-forward recovery crashed: {crashed:?}"
    );
    assert!(
        crashed.iter().any(|c| c.contains("engine.recover.step")),
        "no roll-back recovery crashed: {crashed:?}"
    );
}

// ---------------------------------------------------------------------------------------
// A dead lease holder.

#[test]
fn apply_a_killed_lease_holder_blocks_nothing() {
    let log = tlog!();
    let reference = reference(&log);
    {
        let log = log.case("killed");
        let h = Home2::new(&log);
        let id = h.plan_install_foo(&log);

        log.step("hold");
        let pause = Pause::new(h.home.env.root());
        let child = h
            .cmd(&log, &["apply", &id])
            .failpoints("engine.lock.acquired=pause")
            .pause(&pause)
            .spawn();
        log.reached(&pause, "engine.lock.acquired");

        log.step("the holder line");
        let holder = Lease::holder(&h.lease(&id)).map(|info| (info.pid, info.command));
        log.assert_eq("holder", &holder, &Some((child.pid(), "apply".to_owned())));

        log.step("kill");
        let killed = child.kill();
        log.assert_eq("killed by SIGKILL", &killed.signal, &Some(9));

        log.step("re-apply");
        let started = Instant::now();
        let ran = h.apply(&log, &id);
        let took = started.elapsed();
        ran.ok().assert_private();
        assert!(
            took < Duration::from_secs(5),
            "waited {took:?} for a dead holder"
        );
        log.assert_eq(
            "committed",
            &ran.data()["outcomes"][0]["status"],
            &json!("committed"),
        );
        assert!(matches!(
            oracle(&log, &h, &id, &reference),
            Applied::Once(_)
        ));
        assert!(h.lease(&id).exists(), "apply unlinked its lease file");
    }
    {
        let log = log.case("garbage holder line");
        let h = Home2::new(&log);
        let id = h.plan_install_foo(&log);
        log.step("plant a lease file holding no line");
        fs::write(h.lease(&id), "not json").unwrap();
        log.step("apply");
        let ran = h.apply(&log, &id);
        ran.ok().assert_private();
        assert!(matches!(
            oracle(&log, &h, &id, &reference),
            Applied::Once(_)
        ));
        log.assert_eq(
            "the line overwritten",
            &Lease::holder(&h.lease(&id)).map(|info| info.command),
            &Some("apply".to_owned()),
        );
    }
}

// ---------------------------------------------------------------------------------------
// The reload after the lease, proven.

#[test]
fn apply_reloads_the_plan_after_waiting_for_its_lease() {
    let log = tlog!();
    let reference = reference(&log);
    for case in [
        "edited_while_waiting",
        "removed_while_waiting",
        "unchanged_while_waiting",
    ] {
        let log = log.case(case);
        let h = Home2::new(&log);
        let id = h.plan_install_foo(&log);
        let plan_file = plans_dir(&h.home.env).join(format!("{id}.json"));

        log.step("hold the lease");
        let lease = h.hold_lease(&id);
        log.step("start a waiting applier");
        let waiting = h.cmd(&log, &["apply", &id]).spawn();
        wait_until_open(&log, waiting.pid(), &h.lease(&id), Duration::from_secs(10));

        log.step("change the plan while it waits");
        match case {
            "edited_while_waiting" => {
                let mut plan = read_plan(&h.home.env, &id);
                let argv = plan["display_argv"].as_array_mut().unwrap();
                let last = argv.len() - 1;
                argv[last] = json!("classic");
                let temp = plan_file.with_extension("json.tmp");
                fs::write(&temp, serde_json::to_vec(&plan).unwrap()).unwrap();
                fs::rename(&temp, &plan_file).unwrap();
            }
            "removed_while_waiting" => fs::remove_file(&plan_file).unwrap(),
            _ => {}
        }
        let addons = h.home.addons("_retail_");
        log.tree("addons-retail", &addons);
        log.step("release the lease");
        drop(lease);
        let ran = waiting.wait();
        match case {
            "edited_while_waiting" => {
                ran.fails(3, "plan_not_found");
                log.assert_eq(
                    "reason",
                    &ran.envelope()["error"]["details"]["reason"],
                    &json!("unreadable"),
                );
                log.assert_unchanged("addons-retail", &addons);
            }
            "removed_while_waiting" => {
                ran.fails(3, "plan_not_found");
                let details = &ran.envelope()["error"]["details"];
                log.assert_eq("plan_id", &details["plan_id"], &json!(id));
                log.assert_eq(
                    "path",
                    &details["path"],
                    &json!(plan_file.display().to_string()),
                );
                log.assert_eq("no reason", &details["reason"], &Value::Null);
                log.assert_unchanged("addons-retail", &addons);
            }
            _ => {
                ran.ok().assert_private();
                assert!(matches!(
                    oracle(&log, &h, &id, &reference),
                    Applied::Once(_)
                ));
            }
        }
    }
}

// ---------------------------------------------------------------------------------------
// Injected I/O failures where apply writes or reads first.

#[test]
fn apply_io_failures_before_execution_change_nothing() {
    let log = tlog!();
    let reference = reference(&log);
    for case in [
        "lease_dir_read_only",
        "lease_is_a_directory",
        "lease_is_a_symlink",
        "history_unreadable",
        "history_missing",
    ] {
        let log = log.case(case);
        let h = Home2::new(&log);
        let id = h.plan_install_foo(&log);
        let plans = plans_dir(&h.home.env);
        let history = h.home.state().join("history.jsonl");
        let outside = h.home.env.root().join("outside.txt");
        let mode_of = |path: &Path| fs::metadata(path).map(|m| m.permissions().mode());
        let (plans_mode, history_mode) = (mode_of(&plans).unwrap(), mode_of(&history).ok());
        // A mode change is undone before the trees are compared, and an unreadable file
        // cannot be hashed: those cases read the trees first.
        let chmod = matches!(case, "lease_dir_read_only" | "history_unreadable");
        if chmod {
            for (label, root) in h.trees() {
                log.tree(label, &root);
            }
        }

        log.step("inject");
        match case {
            "lease_dir_read_only" => {
                fs::set_permissions(&plans, fs::Permissions::from_mode(0o500)).unwrap();
                let probe = plans.join("probe");
                assert!(
                    fs::write(&probe, "").is_err(),
                    "this case needs an unprivileged user: {} stayed writable",
                    plans.display()
                );
            }
            "lease_is_a_directory" => fs::create_dir(h.lease(&id)).unwrap(),
            "lease_is_a_symlink" => {
                fs::write(&outside, "outside").unwrap();
                std::os::unix::fs::symlink(&outside, h.lease(&id)).unwrap();
            }
            "history_unreadable" => {
                assert!(history.exists(), "the seed wrote no history");
                fs::set_permissions(&history, fs::Permissions::from_mode(0o000)).unwrap();
                assert!(
                    fs::read(&history).is_err(),
                    "this case needs an unprivileged user: history stayed readable"
                );
            }
            _ => fs::remove_file(&history).unwrap(),
        }
        if !chmod {
            for (label, root) in h.trees() {
                log.tree(label, &root);
            }
        }

        log.step("apply");
        let ran = h.apply(&log, &id);
        if case == "history_missing" {
            ran.ok().assert_private();
            log.note("no history is no evidence: the plan applies");
            assert!(matches!(
                oracle(&log, &h, &id, &reference),
                Applied::Once(_)
            ));
            continue;
        }
        assert_ne!(ran.run.code, 0, "{case}: {}", ran.stdout());
        let code = ran.error_code().unwrap_or_default().to_owned();
        log.json("refused", &ran.envelope()["error"]);
        match case {
            "lease_dir_read_only" | "lease_is_a_directory" | "history_unreadable" => {
                log.assert_eq("code", &code, &"io".to_owned());
            }
            // fsx never opens a lease through a symlink.
            _ => log.assert_eq("code", &code, &"scope_denied".to_owned()),
        }
        // Restore the modes the case changed before reading the trees.
        match (case, history_mode) {
            ("lease_dir_read_only", _) => {
                fs::set_permissions(&plans, fs::Permissions::from_mode(plans_mode)).unwrap();
            }
            ("history_unreadable", Some(mode)) => {
                fs::set_permissions(&history, fs::Permissions::from_mode(mode)).unwrap();
            }
            _ => {}
        }
        for (label, root) in h.trees() {
            if label == "plans" && case == "history_unreadable" {
                // The lease is taken before history is read (M4-D61): its file is the
                // one entry the refusal adds; the plan is as it was.
                let diff = log.diff(label, &root);
                log.assert_eq(
                    "plans: only the lease added",
                    &(
                        diff.added.clone(),
                        diff.removed.is_empty(),
                        diff.changed.is_empty(),
                    ),
                    &(vec![format!("{id}.lease")], true, true),
                );
            } else {
                log.assert_unchanged(label, &root);
            }
        }
        if case == "lease_is_a_symlink" {
            log.assert_eq(
                "the symlink's target",
                &fs::read_to_string(&outside).unwrap(),
                &"outside".to_owned(),
            );
        }
        log.assert_eq(
            "nothing applied",
            &oracle(&log, &h, &id, &reference),
            &Applied::None,
        );
    }
}

// ---------------------------------------------------------------------------------------
// A lost or failed ending.

#[test]
fn apply_after_a_lost_or_failed_ending_is_already_applied() {
    let log = tlog!();
    let reference = reference(&log);
    for case in ["panic_after_commit", "stdout_closed", "error_after_commit"] {
        let log = log.case(case);
        let h = Home2::new(&log);
        let id = h.plan_install_foo(&log);

        log.step("apply");
        match case {
            "panic_after_commit" => {
                let ran = h
                    .cmd(&log, &["apply", &id])
                    .failpoints("engine.retention=panic")
                    .run();
                ran.fails(1, "internal");
                log.assert_eq("command", &ran.envelope()["command"], &json!("apply"));
                let first = ran.envelope()["commands"][0].clone();
                log.assert_eq("the first hint", &first["action"], &json!("undo"));
                let Applied::Once(tx) = oracle(&log, &h, &id, &reference) else {
                    panic!("the panic's commit is not on disk"); // ubs:ignore — the test's own failure
                };
                log.assert_eq(
                    "the hint undoes the committed tx",
                    &first["argv"],
                    &json!(["omaddons", "undo", tx]),
                );
            }
            "stdout_closed" => {
                let mut argv = strings(&["apply", &id, "--json"]);
                argv.extend(strings(&["--progress", "json"]));
                let mut child = h
                    .home
                    .std_command(&argv, &[])
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null())
                    .spawn()
                    .unwrap();
                drop(child.stdout.take());
                let status = child.wait().unwrap();
                log.note(format!("apply with its stdout closed exited with {status}"));
            }
            _ => {
                let ran = h
                    .cmd(&log, &["apply", &id])
                    .failpoints("engine.cleanup.done=error")
                    .run();
                assert_ne!(ran.run.code, 0, "{}", ran.stdout());
            }
        }
        let first = oracle(&log, &h, &id, &reference);
        assert!(matches!(first, Applied::Once(_)), "{case}: {first:?}");
        reapply(&log, &h, &id, &first, &reference);
    }
}
