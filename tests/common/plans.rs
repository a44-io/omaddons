//! Stored-plan fixtures and volatile-id redaction, shared by the plan contract and
//! end-to-end suites. Copies start from a real plan so its typed intent stays valid.
//! The reconciliation helpers (`show`, `apply_with_stdout_closed`, the evidence and
//! history readers, `plant_lines`, `behind_everywhere`) serve `plans show`'s suites and
//! slice 2's end-to-end demo.

use std::fs;
use std::io::{Read as _, Write as _};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{ExitStatus, Stdio};

use jiff::{SignedDuration, Timestamp};
use omaddons::state::paths::Paths;
use serde_json::Value;
use ulid::Ulid;

use super::env::TestEnv;
use super::home::Home;
use super::http::Asset;
use super::tlog::{Ran, TestLog};

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
/// oldest first (history lists them newest first).
pub fn history_txs_for(log: &TestLog, home: &Home, id: &str) -> Vec<(String, String)> {
    let ran = log.run(home, &["history", "--json", "--limit", "200"]);
    ran.ok();
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
