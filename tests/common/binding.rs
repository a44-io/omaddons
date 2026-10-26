//! Preview binding scenarios (D2, M4-D19): previews taken through the test log, the argv
//! of a run bound to one, the exact `plan_stale` refusal, the trees a stale run must
//! leave alone, `<state>` as a dry run must leave it, and the homes the scenarios share
//! (two clients with `gh:o/foo`, an addon one release behind with the memo aged).

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::home::Home;
use super::http::{Asset, gh_release, paths};
use super::tlog::{Ran, Runner, TestLog};

/// `args` as owned strings.
pub fn argv(args: &[&str]) -> Vec<String> {
    args.iter().map(|a| (*a).to_owned()).collect()
}

/// `<args> --dry-run --json` through the log: exit 0, `data.dry_run == true` and a
/// well-formed token (logged as `token:<label>`); returns the run and the token.
#[track_caller]
pub fn preview(log: &TestLog, on: &impl Runner, label: &str, args: &[&str]) -> (Ran, String) {
    let mut run = argv(args);
    run.extend(["--dry-run", "--json"].map(str::to_owned));
    let ran = log.run(on, &run);
    ran.ok();
    let token = ran.data()["precondition"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    log.assert_eq(
        &format!("{label}: a dry run with a token"),
        &(
            &ran.data()["dry_run"],
            omaddons::app::precondition::is_token(&token),
        ),
        &(&json!(true), true),
    );
    log.json(&format!("token:{label}"), &token);
    (ran, token)
}

/// `<args> --if-match <token> --json`.
pub fn bound(args: &[&str], token: &str) -> Vec<String> {
    let mut run = argv(args);
    run.extend(["--if-match", token, "--json"].map(str::to_owned));
    run
}

/// `<args> --dry-run`, with `--dry-run` before any `--`, as a refusal's fix names the
/// preview: `omaddons` first, presentation flags (`--json`) dropped.
pub fn preview_argv(args: &[&str]) -> Vec<String> {
    let mut preview: Vec<String> = std::iter::once("omaddons")
        .chain(args.iter().copied().filter(|a| *a != "--json"))
        .map(str::to_owned)
        .collect();
    if !preview.iter().any(|a| a == "--dry-run") {
        let at = preview
            .iter()
            .position(|a| a == "--")
            .unwrap_or(preview.len());
        preview.insert(at, "--dry-run".to_owned());
    }
    preview
}

/// A refusal of a bound run: exit 1, `plan_stale`, not retryable, `details.changed`
/// exactly `changed` (in token order) and `fix_argv` the preview.
#[track_caller]
pub fn assert_stale(log: &TestLog, ran: &Ran, changed: &[&str], preview: &[String]) {
    let error = &ran.fails(1, "plan_stale").envelope()["error"];
    log.assert_eq(
        "stale: (retryable, details.changed, fix_argv)",
        &(
            &error["retryable"],
            &error["details"]["changed"],
            &error["fix_argv"],
        ),
        &(&json!(false), &json!(changed), &json!(preview)),
    );
}

/// As [`assert_stale`], for a change that may move several parts: `part` is among
/// `details.changed`.
#[track_caller]
pub fn assert_stale_with(log: &TestLog, ran: &Ran, part: &str, preview: &[String]) {
    let error = &ran.fails(1, "plan_stale").envelope()["error"];
    let named = error["details"]["changed"]
        .as_array()
        .is_some_and(|c| c.contains(&json!(part)));
    log.assert_eq(
        &format!("stale: (retryable, {part} among details.changed, fix_argv)"),
        &(&error["retryable"], named, &error["fix_argv"]),
        &(&json!(false), true, &json!(preview)),
    );
}

/// What a stale run must leave alone: every client's AddOns, `<state>/targets` and
/// `<state>/history.jsonl`, recorded when [`guard`] made it.
pub struct Guard {
    label: String,
    clients: Vec<(String, PathBuf)>,
    records: PathBuf,
    history: PathBuf,
    lines: Vec<String>,
}

/// Each client dir under `home`'s Battle.net root (`_retail_`, …) with its AddOns, by
/// folder name.
fn clients(home: &Home) -> Vec<(String, PathBuf)> {
    let mut out: Vec<(String, PathBuf)> = fs::read_dir(&home.root)
        .map(|entries| {
            entries
                .filter_map(|e| {
                    let name = e.ok()?.file_name().to_string_lossy().into_owned();
                    (name.len() > 2 && name.starts_with('_') && name.ends_with('_'))
                        .then(|| (name.clone(), home.addons(&name)))
                })
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out
}

fn history_lines(path: &Path) -> Vec<String> {
    fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

/// Records what a stale run must leave alone, as `<label>:<client> addons`,
/// `<label>:records` and `before:<label>` (the history lines).
pub fn guard(log: &TestLog, home: &Home, label: &str) -> Guard {
    let clients = clients(home);
    for (name, addons) in &clients {
        log.tree(&format!("{label}:{name} addons"), addons);
    }
    let records = home.state().join("targets");
    log.tree(&format!("{label}:records"), &records);
    let history = home.state().join("history.jsonl");
    let lines = history_lines(&history);
    log.json(&format!("before:{label}"), &lines);
    Guard {
        label: label.to_owned(),
        clients,
        records,
        history,
        lines,
    }
}

impl Guard {
    /// Every tree and the history are as [`guard`] recorded them.
    #[track_caller]
    pub fn unchanged(&self, log: &TestLog) {
        for (name, addons) in &self.clients {
            log.assert_unchanged(&format!("{}:{name} addons", self.label), addons);
        }
        log.assert_unchanged(&format!("{}:records", self.label), &self.records);
        log.assert_eq(
            &format!("{}: history.jsonl", self.label),
            &history_lines(&self.history),
            &self.lines,
        );
    }
}

/// `<state>` without `net.log` and its rotation (a dry run logs the requests it makes):
/// each entry's path, with a file's SHA-256, recorded as `state:<label>`.
pub fn state_without_net_log(log: &TestLog, home: &Home, label: &str) -> BTreeMap<String, String> {
    let root = home.state();
    let mut out = BTreeMap::new();
    let mut stack = vec![root.clone()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let rel = path
                .strip_prefix(&root)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            if rel.starts_with("net.log") {
                continue;
            }
            if path.is_dir() {
                stack.push(path);
                out.insert(rel, "dir".to_owned());
            } else {
                let digest: String = Sha256::digest(fs::read(&path).unwrap_or_default())
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect();
                out.insert(rel, digest);
            }
        }
    }
    log.json(&format!("state:{label}"), &out);
    out
}

/// The global lock file's inode, `None` before the first change.
pub fn lock_inode(home: &Home) -> Option<u64> {
    fs::metadata(home.state().join("lock"))
        .ok()
        .map(|m| m.ino())
}

/// Builds for retail and Mists Classic.
pub const MAINLINE_AND_MISTS: &[(&str, u32)] = &[("mainline", 120_105), ("mists", 50_504)];
/// Builds for Classic Era and Anniversary.
pub const ERA_AND_TBC: &[(&str, u32)] = &[("classic", 11_509), ("bcc", 20_506)];

/// Release `tag` of `gh:o/<repo>` as its latest: one asset `<name>.zip` holding
/// `<name>/<name>.toc` for `flavors`' interfaces, at `## Version: <tag>`.
pub fn publish(home: &Home, repo: &str, name: &str, tag: &str, flavors: &[(&str, u32)]) {
    let interfaces: Vec<String> = flavors.iter().map(|(_, i)| i.to_string()).collect();
    let toc = format!(
        "## Interface: {}\n## Version: {tag}\n",
        interfaces.join(", ")
    );
    let (file, path) = (format!("{name}.zip"), format!("{name}/{name}.toc"));
    let assets: [Asset<'_>; 1] = [(file.as_str(), flavors, vec![(path.as_str(), toc.as_str())])];
    gh_release(
        &home.http,
        &format!("https://github.com/o/{repo}"),
        tag,
        &assets,
    );
}

/// A retail and a Mists Classic client, with `gh:o/foo` at `tag` and `gh:o/bar` at v1
/// published for both.
pub fn two_clients_with_foo(tag: &str) -> Home {
    let home = Home::new(&["wow", "wow_classic"]);
    publish(&home, "foo", "Foo", tag, MAINLINE_AND_MISTS);
    publish(&home, "bar", "Bar", "v1", MAINLINE_AND_MISTS);
    home
}

/// Moves every release-memo entry's `fetched_at` back by `by`, so the next `update` asks
/// its sources instead of answering from the 15-minute window.
pub fn age_release_memo(home: &Home, by: jiff::SignedDuration) {
    let (mut memo, _) = omaddons::state::memo::load(&paths(&home.env));
    for entry in memo.entries.values_mut() {
        entry.fetched_at -= by;
    }
    home.seed_memo(&memo);
}

/// Foo installed at `installed` from `gh:o/foo` on `client`, `published` released since
/// for the same `flavors`, and the memo aged an hour: an `update` preview then asks the
/// source and sees `published`.
pub fn foo_behind(
    log: &TestLog,
    home: &Home,
    client: &str,
    (installed, published): (&str, &str),
    flavors: &[(&str, u32)],
) {
    publish(home, "foo", "Foo", installed, flavors);
    log.run(home, &["install", "gh:o/foo", "-t", client, "--json"])
        .ok();
    publish(home, "foo", "Foo", published, flavors);
    age_release_memo(home, jiff::SignedDuration::from_hours(1));
}

/// A `--dry-run` envelope's first target's items, for comparing a preview with its run.
pub fn items(data: &Value) -> Value {
    data["targets"][0]["items"].clone()
}
