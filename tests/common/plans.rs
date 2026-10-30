//! Stored-plan fixtures and volatile-id redaction, shared by the plan contract and
//! end-to-end suites. Copies start from a real plan so its typed intent stays valid.
//! The reconciliation helpers (`show`, `apply_with_stdout_closed`, the evidence and
//! history readers, `plant_lines`, `behind_everywhere`) serve `plans show`'s suites and
//! slice 2's end-to-end demo. `holds_open` and `wait_until_open` find an `apply` holding
//! or waiting for a plan's lease through `/proc/<pid>/fd`.

use std::fs;
use std::io::{Read as _, Write as _};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::time::{Duration, Instant};

use jiff::{SignedDuration, Timestamp};
use omaddons::state::paths::Paths;
use serde_json::{Value, json};
use ulid::Ulid;

use super::env::TestEnv;
use super::home::Home;
use super::http::{Asset, paths};
use super::tlog::{Ran, Runner, TestLog};

/// Replaces each 26-character Crockford run at a word boundary with `<ulid>`.
pub fn redact_ulids(text: &str) -> String {
    let crockford = |c: char| c.is_ascii_digit() || (c.is_ascii_uppercase() && !"ILOU".contains(c));
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        let run = chars[i..].iter().take_while(|c| crockford(**c)).count();
        let boundary = i == 0 || !chars[i - 1].is_ascii_alphanumeric();
        if run == 26 && boundary {
            out.push_str("<ulid>");
            i += 26;
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

/// The plan JSON files, excluding the store lock and per-plan leases, in path order.
pub fn plan_files(env: &TestEnv) -> Vec<PathBuf> {
    let dir = env.cache().join("omaddons/plans");
    let mut paths: Vec<PathBuf> = match fs::read_dir(&dir) {
        Ok(entries) => entries
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
            .collect(),
        Err(error) => {
            assert_eq!(
                error.kind(),
                std::io::ErrorKind::NotFound,
                "list plans in {}: {error}",
                dir.display()
            );
            Vec::new()
        }
    };
    paths.sort();
    paths
}

pub fn plans_dir(env: &TestEnv) -> PathBuf {
    env.cache().join("omaddons/plans")
}

pub fn lease_files(env: &TestEnv) -> Vec<PathBuf> {
    let dir = plans_dir(env);
    if !dir.exists() {
        return Vec::new();
    }
    let mut files: Vec<_> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "lease"))
        .collect();
    files.sort();
    files
}

pub fn read_plan(env: &TestEnv, id: &str) -> Value {
    serde_json::from_slice(&fs::read(plans_dir(env).join(format!("{id}.json"))).unwrap()).unwrap()
}

/// `<cache>/omaddons/plans/<id>.lease`, the lease a real `apply` of plan `id` holds.
pub fn lease_path(env: &TestEnv, id: &str) -> PathBuf {
    plans_dir(env).join(format!("{id}.lease"))
}

/// What process `pid` has open: the targets of the links under `/proc/<pid>/fd/`.
fn open_files(pid: u32) -> Vec<PathBuf> {
    fs::read_dir(format!("/proc/{pid}/fd"))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|fd| fs::read_link(fd.path()).ok())
        .collect()
}

/// Whether process `pid` has `path` open (an `apply` holding or waiting for a plan's
/// lease keeps its lease file open). A path that does not exist is open nowhere.
pub fn holds_open(pid: u32, path: &Path) -> bool {
    fs::canonicalize(path).is_ok_and(|wanted| open_files(pid).contains(&wanted))
}

/// Waits (`limit` at most, polling every 10 ms) until process `pid` has `path` open,
/// logging each poll; panics after `limit` naming the pid, the path and the links seen.
#[track_caller]
pub fn wait_until_open(log: &TestLog, pid: u32, path: &Path, limit: Duration) {
    let deadline = Instant::now() + limit;
    loop {
        let open = holds_open(pid, path);
        log.note(format!(
            "pid {pid} has {} open: {open}",
            path.file_name().unwrap_or_default().to_string_lossy()
        ));
        if open {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "pid {pid} never opened {} within {limit:?}; its links: {:?}",
            path.display(),
            open_files(pid)
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Physical counts, including files beyond the library listing's bounded window.
pub fn store_listing(env: &TestEnv) -> (usize, usize, u64) {
    let plans = plan_files(env);
    let bytes = plans
        .iter()
        .map(|path| fs::metadata(path).unwrap().len())
        .sum();
    (plans.len(), lease_files(env).len(), bytes)
}

pub fn edit_plan(env: &TestEnv, id: &str, edit: impl FnOnce(&mut Value)) {
    let mut plan = read_plan(env, id);
    edit(&mut plan);
    fs::write(
        plans_dir(env).join(format!("{id}.json")),
        serde_json::to_vec(&plan).unwrap(),
    )
    .unwrap();
}

/// Oldest first, with distinct fixed-time ids and a millisecond between creations.
pub fn plant_expired(env: &TestEnv, template: &Value, n: usize) {
    let created: Timestamp = template["created_at"].as_str().unwrap().parse().unwrap();
    let start = created.checked_sub(SignedDuration::from_hours(48)).unwrap();
    for index in 0..n {
        let at = start
            .checked_add(SignedDuration::from_millis(i64::try_from(index).unwrap()))
            .unwrap();
        let id = Ulid::from_parts(
            u64::try_from(at.as_millisecond()).unwrap(),
            u128::try_from(index + 1).unwrap(),
        );
        age_plan(env, template, id, at);
    }
}

/// Writes a copy at `id`, dated `created_at`, with the ordinary 24-hour lifetime.
/// Everything else in the real plan is preserved, and the copy remains owner-only.
pub fn age_plan(env: &TestEnv, template: &Value, id: Ulid, created_at: Timestamp) {
    let mut plan = template.clone();
    plan["plan_id"] = Value::from(id.to_string());
    plan["created_at"] = Value::from(created_at.to_string());
    plan["expires_at"] = Value::from(
        created_at
            .checked_add(SignedDuration::from_hours(24))
            .unwrap()
            .to_string(),
    );
    let dir = env.cache().join("omaddons/plans");
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{id}.json"));
    fs::write(&path, serde_json::to_vec(&plan).unwrap()).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    let loaded = omaddons::state::plans::load(&path).unwrap();
    assert_eq!(
        loaded.plan_id.to_string(),
        id.to_string(),
        "planted plan must load: {}",
        path.display()
    );
}

/// Plants `n` live copies of `template`, each with `padding` extra summary bytes.
/// Time comes from the real plan so a test with `OMADDONS_NOW` remains deterministic.
pub fn plant_live(env: &TestEnv, template: &Value, n: usize, padding: usize) {
    let created_at = template["created_at"].as_str().unwrap().parse().unwrap();
    let mut plan = template.clone();
    plan["summary"]
        .as_array_mut()
        .unwrap()
        .push(Value::from("x".repeat(padding)));
    for _ in 0..n {
        age_plan(env, &plan, Ulid::generate(), created_at);
    }
}

// ---------------------------------------------------------------------------------------
// Reconciling a plan from history (`plans show`).

/// The label of the tree [`read_only`] hashes around each run.
const WHOLE_HOME: &str = "whole home";

/// `data.plan_id` of a dry run that stored a plan; fails the test when it stored none.
pub fn plan_id(ran: &Ran) -> String {
    match ran.data()["plan_id"].as_str() {
        Some(id) => id.to_owned(),
        None => panic!("the dry run stored no plan: {}", ran.data()), // ubs:ignore — the assertion
    }
}

/// `(target, tx)` of each `outcomes[]` entry an `apply` reports `committed`, in order.
pub fn committed(ran: &Ran) -> Vec<(String, String)> {
    ran.data()["outcomes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|o| o["status"] == "committed")
        .map(|o| text_pair(&o["target"], &o["tx"]))
        .collect()
}

/// `(target, tx)` of `plans show`'s `data.evidence.targets`, oldest first.
pub fn evidence_txs(ran: &Ran) -> Vec<(String, String)> {
    ran.data()["evidence"]["targets"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|t| text_pair(&t["target"], &t["tx"]))
        .collect()
}

/// `(target id, tx)` of the `history --json --limit 200` entries whose `plan` is `id`,
/// oldest first (history lists them newest first). The run names no account, realm or
/// character on either stream.
pub fn history_txs_for(log: &TestLog, on: &impl Runner, id: &str) -> Vec<(String, String)> {
    let ran = log.run(
        on,
        &["history", "--json", "--limit", "200", "--progress", "json"],
    );
    ran.ok().assert_private();
    let mut lines: Vec<(String, String)> = ran.data()["entries"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|e| e["plan"] == id)
        .map(|e| text_pair(&e["target"]["id"], &e["tx"]))
        .collect();
    lines.reverse();
    log.json("history lines carrying the plan", &lines);
    lines
}

fn text_pair(a: &Value, b: &Value) -> (String, String) {
    let text = |v: &Value| v.as_str().unwrap_or_default().to_owned();
    (text(a), text(b))
}

/// Runs `omaddons plans show <args> --json` with `env`'s variables (`OMADDONS_NOW`,
/// `OMADDONS_OFFLINE`) through [`read_only`], so every run proves it wrote nothing and
/// asked nothing.
pub fn show(log: &TestLog, home: &Home, args: &[&str], env: &[(&str, &str)]) -> Ran {
    let mut argv = vec!["plans", "show"];
    argv.extend_from_slice(args);
    argv.push("--json");
    read_only(log, home, &argv, env)
}

/// [`show`] without `--json`, for the human view.
pub fn show_human(log: &TestLog, home: &Home, args: &[&str], env: &[(&str, &str)]) -> Ran {
    let mut argv = vec!["plans", "show"];
    argv.extend_from_slice(args);
    read_only(log, home, &argv, env)
}

/// Runs `omaddons <argv>` with `env`'s variables and fails the test unless the run left
/// the whole home's tree hash-identical (state, cache, config and every client) and
/// added no `net.log` line. The argv is used verbatim.
pub fn read_only(log: &TestLog, home: &Home, argv: &[&str], env: &[(&str, &str)]) -> Ran {
    let root = home.env.root();
    let before = net_log_lines(home);
    log.tree(WHOLE_HOME, root);
    let mut cmd = log.cmd(home, argv);
    for (key, value) in env {
        cmd = cmd.env(key, value);
    }
    let ran = cmd.run();
    log.assert_unchanged(WHOLE_HOME, root);
    log.assert_eq("net.log gained no line", &net_log_lines(home), &before);
    ran
}

/// The lines of `net.log.1` and `net.log`, raw; a missing file has none.
fn net_log_lines(home: &Home) -> Vec<String> {
    ["net.log.1", "net.log"]
        .iter()
        .flat_map(|name| {
            let bytes = fs::read(home.state().join(name)).unwrap_or_default();
            String::from_utf8_lossy(&bytes)
                .lines()
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .collect()
}

/// Runs `omaddons apply <id> --json` with its stdout's read end closed as soon as it
/// starts, so the envelope meets a closed pipe after the commits: an agent whose
/// `apply` output was lost. Records the argv, the exit status and stderr in `log`
/// (`log.run` cannot do this: it reads stdout to the end); returns the status, which the
/// caller only records.
pub fn apply_with_stdout_closed(log: &TestLog, home: &Home, id: &str) -> ExitStatus {
    let argv: Vec<String> = ["apply", id, "--json"].map(str::to_owned).to_vec();
    let mut child = home
        .std_command(&argv, &[])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    drop(child.stdout.take());
    let mut stderr = String::new();
    if let Some(mut pipe) = child.stderr.take() {
        pipe.read_to_string(&mut stderr).unwrap();
    }
    let status = child.wait().unwrap();
    log.note(format!(
        "omaddons {} with stdout closed: {status}",
        argv.join(" ")
    ));
    let events: Vec<Value> = stderr
        .lines()
        .map(|line| serde_json::from_str(line).unwrap_or_else(|_| Value::from(line)))
        .collect();
    log.json("stderr of the apply whose stdout was closed", &events);
    status
}

/// Appends `n` copies of `line`, each with its newline, to `history.jsonl`: a fixture's
/// write, outside `src/`.
pub fn plant_lines(paths: &Paths, line: &str, n: usize) {
    let mut bytes = Vec::with_capacity((line.len() + 1) * n);
    for _ in 0..n {
        bytes.extend_from_slice(line.as_bytes());
        bytes.push(b'\n');
    }
    fs::OpenOptions::new()
        .append(true)
        .open(paths.history_file())
        .unwrap()
        .write_all(&bytes)
        .unwrap();
}

/// `gh:o/foo`, the addon [`behind_everywhere`] installs.
pub const FOO: &str = "https://github.com/o/foo";

/// Foo's flavors in its `release.json`: retail's and Classic Era's interfaces.
const FOO_FLAVORS: &[(&str, u32)] = &[("mainline", 120_105), ("classic", 11_509)];

/// Foo `v1` and `v2`: one folder `Foo` with a TOC for each client.
const FOO_V1: &[(&str, &str)] = &[
    (
        "Foo/Foo_Mainline.toc",
        "## Interface: 120105\n## Title: Foo\n## Version: v1\nFoo.lua\n",
    ),
    (
        "Foo/Foo_Vanilla.toc",
        "## Interface: 11509\n## Title: Foo\n## Version: v1\nFoo.lua\n",
    ),
    ("Foo/Foo.lua", "local version = 1\n"),
];
const FOO_V2: &[(&str, &str)] = &[
    (
        "Foo/Foo_Mainline.toc",
        "## Interface: 120105\n## Title: Foo\n## Version: v2\nFoo.lua\n",
    ),
    (
        "Foo/Foo_Vanilla.toc",
        "## Interface: 11509\n## Title: Foo\n## Version: v2\nFoo.lua\n",
    ),
    ("Foo/Foo.lua", "local version = 2\n"),
];

fn foo_asset(entries: &'static [(&'static str, &'static str)]) -> [Asset<'static>; 1] {
    [("Foo.zip", FOO_FLAVORS, entries.to_vec())]
}

/// Retail and Classic Era (`retail`, `era`), each running Foo `v1` of `gh:o/foo` while
/// `v2` is its latest release, with the release memo a day old: an `update` preview
/// approves Foo v1 → v2 on both clients, and its `apply` commits two transactions in one
/// run. The home's environment is registered with `log`.
pub fn behind_everywhere(log: &TestLog) -> Home {
    let mut home = Home::new(&["wow", "wow_classic_era"]);
    log.env(&home);
    home.installed_behind(
        FOO,
        ("v1", &foo_asset(FOO_V1)),
        ("v2", &foo_asset(FOO_V2)),
        &["retail", "era"],
    );
    home
}

// ---------------------------------------------------------------------------------------
// One stored plan's lifecycle (D7, for every storable command; E7 runs it for a repair
// undo).

/// What a second `apply` of an applied plan says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Second {
    /// History records the plan: `plan_stale`, `already_applied`, naming the real run's
    /// transactions, also past the plan's expiry.
    AlreadyApplied,
    /// The command writes no history line (`backup prune`): its own binding refuses the
    /// plan (`selection` moved), then, past its expiry, `expired`.
    StaleSelection,
}

/// One plan's lifecycle to run: a home's storable change and what bounds it.
pub struct LifecycleCase<'a> {
    /// The command path, in step labels and messages.
    pub label: &'a str,
    /// The storable argv, without `--dry-run` and `--json`.
    pub preview: Vec<String>,
    /// Labelled trees every refusal leaves unchanged.
    pub roots: Vec<(String, PathBuf)>,
    /// Set on every run unless a step overrides it.
    pub env: Vec<(String, String)>,
    /// The home has a `WTF/`: every run with `--progress json`, and `assert_private`.
    pub progress_json: bool,
    pub second: Second,
}

/// What the lifecycle's real run committed: its plan, and its transactions in commit
/// order (none for `backup prune`).
pub struct Lifecycle {
    pub plan_id: String,
    pub txs: Vec<String>,
}

/// What a refusal must leave as it was, besides the case's roots: the plan files' names
/// and bytes, and history's length.
#[derive(Debug, PartialEq, Eq, serde::Serialize)]
struct Witness {
    plan_files: Vec<(String, String)>,
    history_lines: usize,
}

impl Witness {
    fn of(env: &TestEnv) -> Self {
        Self {
            plan_files: plan_files(env)
                .iter()
                .map(|path| {
                    let name = path.file_name().unwrap().to_string_lossy().into_owned();
                    (name, sha256(&fs::read(path).unwrap()))
                })
                .collect(),
            history_lines: fs::read_to_string(paths(env).history_file())
                .map(|text| text.lines().count())
                .unwrap_or(0),
        }
    }
}

fn sha256(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Every file under `<state>/omaddons` but `net.log` and `net.log.1`, by relative path,
/// with its bytes' digest (`dir` for a directory).
fn state_but_net_log(env: &TestEnv) -> Vec<(String, String)> {
    fn walk(root: &std::path::Path, dir: &std::path::Path, out: &mut Vec<(String, String)>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries {
            let path = entry.unwrap().path();
            let rel = path.strip_prefix(root).unwrap().display().to_string();
            if path.is_dir() {
                out.push((rel, "dir".to_owned()));
                walk(root, &path, out);
            } else if !matches!(rel.as_str(), "net.log" | "net.log.1") {
                out.push((rel, sha256(&fs::read(&path).unwrap())));
            }
        }
    }
    let root = env.state().join("omaddons");
    let mut out = Vec::new();
    walk(&root, &root, &mut out);
    out.sort();
    out
}

fn words(value: &Value) -> Vec<String> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|word| word.as_str().map(str::to_owned))
        .collect()
}

/// `omaddons <args>` for `case`: `--json` (and `--progress json` when the home has a
/// `WTF/`) before any `--`, the case's variables, and `now` as `OMADDONS_NOW` when given.
fn lifecycle_run(
    log: &TestLog,
    on: &impl Runner,
    case: &LifecycleCase<'_>,
    args: &[String],
    now: Option<&str>,
) -> Ran {
    let mut argv = args.to_vec();
    let at = argv
        .iter()
        .position(|word| word == "--")
        .unwrap_or(argv.len());
    let mut flags = vec!["--json".to_owned()];
    if case.progress_json {
        flags.extend(["--progress".to_owned(), "json".to_owned()]);
    }
    argv.splice(at..at, flags);
    let mut cmd = log.cmd(on, &argv);
    for (key, value) in &case.env {
        cmd = cmd.env(key, value);
    }
    if let Some(now) = now {
        cmd = cmd.env("OMADDONS_NOW", now);
    }
    let ran = cmd.run();
    if case.progress_json {
        ran.assert_private();
    }
    ran
}

/// A lifecycle refusal: `plan_stale` with `reason`, `changed: []`, not retryable, and
/// the plan's preview as its fix.
#[track_caller]
fn refused_with(log: &TestLog, case: &LifecycleCase<'_>, ran: &Ran, reason: &str, fix: &Value) {
    ran.fails(1, "plan_stale");
    let error = &ran.envelope()["error"];
    log.json("refusal", error);
    let label = case.label;
    log.assert_eq(
        &format!("{label}: reason"),
        &error["details"]["reason"],
        &json!(reason),
    );
    log.assert_eq(
        &format!("{label}: changed"),
        &error["details"]["changed"],
        &json!([]),
    );
    log.assert_eq(
        &format!("{label}: not retryable"),
        &error["retryable"],
        &json!(false),
    );
    log.assert_eq(
        &format!("{label}: the fix previews again"),
        &error["fix_argv"],
        fix,
    );
}

/// An unreadable plan file: `plan_not_found`, `reason: "unreadable"`, naming the file.
#[track_caller]
fn unreadable(log: &TestLog, case: &LifecycleCase<'_>, ran: &Ran, path: &std::path::Path) {
    ran.fails(3, "plan_not_found");
    let details = &ran.envelope()["error"]["details"];
    log.json("refusal details", details);
    log.assert_eq(
        &format!("{}: unreadable", case.label),
        &details["reason"],
        &json!("unreadable"),
    );
    log.assert_eq(
        &format!("{}: the plan file", case.label),
        &details["path"],
        &json!(path.display().to_string()),
    );
}

/// One stored plan's lifecycle, end to end: the preview; `apply --dry-run` while the test
/// holds the global lock and the plan's lease; the refusals of an expired plan, one from
/// another build, an unknown option in its intent and a `display_argv` that disagrees
/// (each hand edit undone byte for byte before the next step); the real `apply`; and the
/// second. Every refusal leaves the case's roots, the plan files and history as they
/// were.
pub fn lifecycle(log: &TestLog, on: &impl Runner, case: &LifecycleCase<'_>) -> Lifecycle {
    let env = on.test_env();
    let label = case.label;
    let step = |name: &str| log.step(&format!("{label}: {name}"));
    let strings =
        |words: &[&str]| -> Vec<String> { words.iter().map(|w| (*w).to_owned()).collect() };
    let unchanged = |witness: &Witness| {
        for (root_label, root) in &case.roots {
            log.assert_unchanged(root_label, root);
        }
        log.assert_eq(
            &format!("{label}: plan files and history unchanged"),
            &Witness::of(env),
            witness,
        );
    };

    step("preview");
    let mut argv = case.preview.clone();
    argv.push("--dry-run".to_owned());
    let previewed = lifecycle_run(log, on, case, &argv, None);
    previewed.ok();
    let id = plan_id(&previewed);
    let plan = read_plan(env, &id);
    log.json("plan file", &plan);
    for (root_label, root) in &case.roots {
        log.tree(root_label, root);
    }
    let witness = Witness::of(env);
    log.json("plan files and history", &witness);
    let path = plans_dir(env).join(format!("{id}.json"));
    let original = fs::read(&path).unwrap();
    let fix = json!(omaddons::app::precondition::preview_argv(&words(
        &plan["display_argv"]
    )));
    let expires: Timestamp = plan["expires_at"].as_str().unwrap().parse().unwrap();
    let past = expires
        .checked_add(SignedDuration::from_secs(60))
        .unwrap()
        .to_string();

    step("apply --dry-run under held locks");
    let lock_paths = paths(env);
    let global = omaddons::fsx::GlobalLock::acquire(&lock_paths, "lifecycle").unwrap();
    let scopes = omaddons::fsx::ScopeTable::new(&lock_paths, &[], None).unwrap();
    let plan_key: omaddons::model::PlanId = id.parse().unwrap();
    let lease_path = lock_paths.plan_lease(&plan_key);
    let lease = omaddons::fsx::Lease::acquire(
        &scopes,
        &lease_path,
        "lifecycle",
        std::time::Duration::from_secs(5),
    )
    .map_err(|_| "the plan's lease")
    .unwrap();
    log.note(format!(
        "the test holds the global lock and the plan's lease ({})",
        lease_path.strip_prefix(env.root()).unwrap().display()
    ));
    let state = state_but_net_log(env);
    let dry = lifecycle_run(log, on, case, &strings(&["apply", &id, "--dry-run"]), None);
    lease.release();
    global.release();
    log.note("the test released the global lock and the plan's lease");
    dry.ok();
    log.assert_eq(
        &format!("{label}: dry run under locks, elapsed_ms < 4000"),
        &(dry.elapsed_ms < 4000),
        &true,
    );
    let data = dry.data();
    log.assert_eq(
        &format!("{label}: the stored approval"),
        &data["approved"],
        &plan["approval"],
    );
    log.assert_eq(
        &format!("{label}: a dry run"),
        &data["result"]["dry_run"],
        &json!(true),
    );
    let outcomes: Vec<(String, Value)> = data["outcomes"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|o| {
            (
                o["status"].as_str().unwrap_or_default().to_owned(),
                o["tx"].clone(),
            )
        })
        .collect();
    assert!(!outcomes.is_empty(), "{label}: no outcome: {data}");
    log.assert_eq(
        &format!("{label}: every outcome unchanged, no tx"),
        &outcomes,
        &vec![("unchanged".to_owned(), Value::Null); outcomes.len()],
    );
    let hint = dry
        .commands()
        .into_iter()
        .find(|c| c.action == "apply")
        .map(|c| c.argv);
    log.assert_eq(
        &format!("{label}: the apply hint"),
        &hint,
        &Some(strings(&["omaddons", "apply", &id])),
    );
    unchanged(&witness);
    log.assert_eq(
        &format!("{label}: <state> unchanged bar net.log"),
        &state_but_net_log(env),
        &state,
    );

    step("expired");
    let ran = lifecycle_run(log, on, case, &strings(&["apply", &id]), Some(&past));
    refused_with(log, case, &ran, "expired", &fix);
    unchanged(&witness);

    // Each hand edit, its refusal, and the original bytes written back.
    let edited = |name: &str, edit: &dyn Fn(&mut Value), check: &dyn Fn(&Ran)| {
        step(name);
        let mut value: Value = serde_json::from_slice(&original).unwrap();
        edit(&mut value);
        fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        let ran = lifecycle_run(log, on, case, &strings(&["apply", &id]), None);
        check(&ran);
        fs::write(&path, &original).unwrap();
        log.note(format!("plan file restored: {}", sha256(&original)));
        unchanged(&witness);
    };
    edited(
        "another build",
        &|plan| {
            plan["build_id"] = json!("0000000000000000");
            log.json("plan file", &json!({"build_id": plan["build_id"]}));
        },
        &|ran| refused_with(log, case, ran, "binary_changed", &fix),
    );
    edited(
        "an unknown option in this intent",
        &|plan| {
            plan["intent"]["bogus"] = json!(true);
            log.json("plan file", &json!({"intent.bogus": true}));
        },
        &|ran| unreadable(log, case, ran, &path),
    );
    edited(
        "a display_argv that disagrees",
        &|plan| {
            let argv = plan["display_argv"].as_array_mut().unwrap();
            *argv.last_mut().unwrap() = json!("edited");
            log.json("plan file", &json!({"display_argv": plan["display_argv"]}));
        },
        &|ran| unreadable(log, case, ran, &path),
    );

    step("apply");
    let applied = lifecycle_run(log, on, case, &strings(&["apply", &id]), None);
    applied.ok();
    log.json("outcomes", &applied.data()["outcomes"]);
    let statuses: Vec<String> = applied.data()["outcomes"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|o| o["status"].as_str().unwrap_or_default().to_owned())
        .collect();
    log.assert_eq(
        &format!("{label}: every outcome committed"),
        &statuses,
        &vec!["committed".to_owned(); statuses.len()],
    );
    let mut reported: Vec<String> = committed(&applied)
        .into_iter()
        .map(|(_, tx)| tx)
        .filter(|tx| !tx.is_empty())
        .collect();
    let txs: Vec<String> = history_txs_for(log, on, &id)
        .into_iter()
        .map(|(_, tx)| tx)
        .collect();
    log.json("evidence", &txs);
    let mut recorded = txs.clone();
    reported.sort();
    recorded.sort();
    log.assert_eq(&format!("{label}: txs"), &recorded, &reported);
    for (root_label, root) in &case.roots {
        log.tree(root_label, root);
    }
    let witness = Witness::of(env);

    step("second apply");
    let again = lifecycle_run(log, on, case, &strings(&["apply", &id]), None);
    let late = lifecycle_run(log, on, case, &strings(&["apply", &id]), Some(&past));
    match case.second {
        Second::AlreadyApplied => {
            for ran in [&again, &late] {
                refused_with(log, case, ran, "already_applied", &fix);
                log.assert_eq(
                    &format!("{label}: the real run's txs"),
                    &ran.envelope()["error"]["details"]["txs"],
                    &json!(txs),
                );
            }
        }
        Second::StaleSelection => {
            again.fails(1, "plan_stale");
            let details = &again.envelope()["error"]["details"];
            log.json("refusal details", details);
            let changed = words(&details["changed"]);
            log.assert_eq(
                &format!("{label}: the selection moved"),
                &changed.contains(&"selection".to_owned()),
                &true,
            );
            log.assert_eq(
                &format!("{label}: no lifecycle reason"),
                &details.get("reason").cloned().unwrap_or(Value::Null),
                &Value::Null,
            );
            refused_with(log, case, &late, "expired", &fix);
        }
    }
    unchanged(&witness);
    Lifecycle { plan_id: id, txs }
}
