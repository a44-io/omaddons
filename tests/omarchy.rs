//! `omaddons omarchy install` end to end (E2, omad-widget-plugin-3bcbh.9): a fake
//! `<config-omarchy>` and stub `omarchy-*` tools that log their argv and `OMARCHY_PATH`.
//! omaddons runs with `PATH` holding the stubs alone, so the real tools (whose
//! `omarchy-bar put` would place the widget on this machine's bar) are never reachable,
//! and adds the widget from a local export of its repository through the
//! `omarchy-plugin-add` stub, never from the network.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use common::env::TestEnv;
use common::home::{Run, wait};
use common::pause::Pause;
use common::privacy::assert_private_all;
use serde_json::Value;

const ID: &str = "a44.omaddons";

/// One desktop: the environment, the stubs and their log.
struct Desk {
    env: TestEnv,
}

impl Desk {
    /// Omarchy's config dir, its shell (on `PATH`) and every tool, all answering well.
    fn new() -> Self {
        let desk = Self::bare();
        fs::create_dir_all(desk.config_omarchy()).unwrap();
        for tool in common::omarchy::TOOLS {
            desk.tool(tool);
        }
        desk
    }

    /// No Omarchy at all: an empty stub dir and an `OMARCHY_PATH` without `shell/`. The
    /// widget's export is there, for an install that gets that far.
    fn bare() -> Self {
        let env = TestEnv::new();
        fs::create_dir_all(env.root().join("bin")).unwrap();
        fs::create_dir_all(env.root().join("omarchy")).unwrap();
        common::omarchy::write_widget_repo(&env.root().join("widget-repo"));
        Self { env }
    }

    /// The local export `omarchy install` adds the widget from.
    fn repo(&self) -> PathBuf {
        self.root().join("widget-repo")
    }

    /// The widget's checkout as the stub's clone of the export leaves it.
    fn checkout(&self) -> BTreeMap<String, Vec<u8>> {
        tree(&self.repo())
    }

    /// A widget an older omaddons rendered in the widget's folder, for this run's
    /// directories.
    fn rendered(&self) {
        common::omarchy::plant_rendered(
            &self.plugin(),
            Path::new("/opt/omaddons-0.2.1/omaddons"),
            &common::omarchy::context_json(&self.env),
        );
    }

    fn root(&self) -> &Path {
        self.env.root()
    }

    fn config_omarchy(&self) -> PathBuf {
        self.env.config().join("omarchy")
    }

    fn plugins(&self) -> PathBuf {
        self.config_omarchy().join("plugins")
    }

    fn plugin(&self) -> PathBuf {
        self.plugins().join(ID)
    }

    /// Where builds before omad-o8g4 installed the widget.
    fn legacy(&self) -> PathBuf {
        self.plugins().join("omaddons")
    }

    /// A plugin folder of `name` holding a manifest with `id` and one more file.
    fn planted(&self, name: &str, id: &str) -> PathBuf {
        let dir = self.plugins().join(name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("manifest.json"),
            format!(r#"{{"schemaVersion": 1, "id": "{id}", "version": "0.0.1"}}"#),
        )
        .unwrap();
        fs::write(dir.join("Panel.qml"), "Item {}\n").unwrap();
        dir
    }

    fn history(&self) -> PathBuf {
        self.env.state().join("omaddons/history.jsonl")
    }

    fn doorbell(&self) -> PathBuf {
        self.env.cache().join("omaddons/doorbell.json")
    }

    fn log(&self) -> PathBuf {
        self.root().join("tools.log")
    }

    /// A stub tool, as `common::omarchy::write_tool` writes it.
    fn tool(&self, name: &str) {
        common::omarchy::write_tool(self.root(), name);
    }

    fn set(&self, file: &str, value: &str) {
        fs::write(self.root().join(file), value).unwrap();
    }

    /// `shell.json` with the widget in `section`, as the owner placed it.
    fn placed(&self, section: &str) {
        fs::write(
            self.config_omarchy().join("shell.json"),
            format!(
                r#"{{"bar":{{"layout":{{"left":[{{"id":"omarchy.menu"}}],"{section}":[{{"id":"{ID}"}}]}}}}}}"#
            ),
        )
        .unwrap();
    }

    fn command(
        &self,
        binary: &Path,
        args: &[&str],
        extra: &[(&str, &str)],
    ) -> std::process::Command {
        let mut cmd = std::process::Command::new(binary); // ubs:ignore — the binary under test, or its copy
        self.env.apply_to(&mut cmd);
        cmd.env("PATH", self.root().join("bin"));
        cmd.env("OMARCHY_PATH", self.root().join("omarchy"));
        cmd.env(common::omarchy::WIDGET_REPO_ENV, self.repo());
        for (key, value) in extra {
            cmd.env(key, value);
        }
        cmd.args(args);
        cmd
    }

    /// `omaddons <args> --json` with this desk's environment and `extra`.
    fn run_with(&self, args: &[&str], extra: &[(&str, &str)]) -> Run {
        self.run_binary(Path::new(env!("CARGO_BIN_EXE_omaddons")), args, extra)
    }

    fn run_binary(&self, binary: &Path, args: &[&str], extra: &[(&str, &str)]) -> Run {
        let mut argv: Vec<&str> = args.to_vec();
        argv.push("--json");
        let output = self.command(binary, &argv, extra).output().unwrap();
        let run = Run {
            argv: argv.iter().map(|a| (*a).to_owned()).collect(),
            code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8(output.stdout).unwrap(),
            stderr: String::from_utf8(output.stderr).unwrap(),
        };
        assert_private_all(&run.stdout, &run.stderr);
        run
    }

    fn run(&self, args: &[&str]) -> Run {
        self.run_with(args, &[])
    }

    fn install(&self) -> Value {
        self.run(&["omarchy", "install"]).expect(0)
    }

    /// The stub log's lines, `OMARCHY_PATH` lines checked and dropped.
    fn calls(&self) -> Vec<String> {
        let text = fs::read_to_string(self.log()).unwrap_or_default();
        let mut calls = Vec::new();
        for pair in text.lines().collect::<Vec<_>>().chunks(2) {
            let fake = self.root().join("omarchy");
            assert_eq!(
                pair[1],
                format!("OMARCHY_PATH={}", fake.display()),
                "{text}"
            );
            calls.push(pair[0].to_owned());
        }
        calls
    }

    fn widget_lines(&self) -> Vec<Value> {
        fs::read_to_string(self.history())
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .filter(|line| line["kind"] == "widget")
            .collect()
    }

    /// The `<plugins>` entries other than the widget (an emptied prune trash goes too).
    fn leftovers(&self) -> Vec<String> {
        let Ok(entries) = fs::read_dir(self.plugins()) else {
            return Vec::new();
        };
        let mut names: Vec<String> = entries
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .filter(|n| n != ID)
            .collect();
        names.sort();
        names
    }

    fn inflight_locks(&self) -> usize {
        fs::read_dir(self.env.state().join("omaddons/inflight"))
            .map(|entries| entries.count())
            .unwrap_or(0)
    }
}

/// Every file under `dir`, by relative path.
fn tree(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut files = BTreeMap::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(current) = pending.pop() {
        for entry in fs::read_dir(&current).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else {
                let rel = path.strip_prefix(dir).unwrap().display().to_string();
                files.insert(rel, fs::read(&path).unwrap());
            }
        }
    }
    files
}

fn this_binary() -> PathBuf {
    fs::canonicalize(env!("CARGO_BIN_EXE_omaddons")).unwrap()
}

/// The widget manifest's `version`, as the export holds it (the publish stamps it).
fn repo_version() -> String {
    let manifest: Value = serde_json::from_str(include_str!("../omarchy/manifest.json")).unwrap();
    manifest["version"].as_str().unwrap().to_owned()
}

/// The widget's repository added through Omarchy (`omarchy-plugin-add <url> --yes`, never
/// `--enable`), recorded as a `widget` history line, then placed with `omarchy-bar put`.
/// omaddons writes nothing of the widget's code: the folder is exactly the clone.
#[test]
fn a_fresh_install_adds_the_repository_through_omarchy_and_places_it() {
    let desk = Desk::new();
    let d = desk.install();
    let data = &d["data"];
    assert_eq!(data["dry_run"], false);
    assert_eq!(data["installed"], "fresh", "{d}");
    assert_eq!(data["enabled"], "yes", "{d}");
    assert_eq!(data["section"], "right");
    let plugin = &data["plugin"];
    assert_eq!(plugin["installed"], true);
    assert_eq!(plugin["id"], ID);
    assert_eq!(plugin["path"], desk.plugin().display().to_string());
    assert_eq!(plugin["version"], repo_version());
    assert_eq!(
        plugin["binary"],
        Value::Null,
        "a checkout finds omaddons itself"
    );
    assert_eq!(plugin["outdated"], false);
    assert_eq!(plugin["enabled"], true);
    assert_eq!(plugin["section"], "right");
    assert_eq!(d["warnings"], serde_json::json!([]));

    assert_eq!(
        desk.calls(),
        [
            format!("omarchy-plugin-add {} --yes", desk.repo().display()),
            format!("omarchy-bar put {ID} --section right"),
        ]
    );
    assert_eq!(tree(&desk.plugin()), desk.checkout(), "exactly the clone");
    let lines = desk.widget_lines();
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["target"], Value::Null);
    assert_eq!(lines[0]["ops"], serde_json::json!([]));
    let bell: Value = serde_json::from_slice(&fs::read(desk.doorbell()).unwrap()).unwrap();
    assert_eq!(bell["command"], "omarchy install");
    assert_eq!(desk.leftovers(), Vec::<String>::new());
    assert_eq!(desk.inflight_locks(), 0);
}

/// The repository is the widget's own unless `OMADDONS_WIDGET_REPO` says otherwise; a
/// value that is empty or reads as an option is refused before anything runs.
#[test]
fn the_repository_is_the_widgets_and_a_bad_override_is_refused() {
    let desk = Desk::new();
    for bad in ["", "--upload-pack=touch /tmp/x"] {
        let d = desk
            .run_with(
                &["omarchy", "install"],
                &[(common::omarchy::WIDGET_REPO_ENV, bad)],
            )
            .expect(2);
        assert_eq!(d["error"]["code"], "usage", "{bad}: {d}");
        assert!(!desk.plugin().exists());
        assert_eq!(desk.calls(), Vec::<String>::new());
    }
    // Unset, the widget's repository on GitHub: the stub's clone of it fails here, as no
    // network is reachable from the stub, and nothing is left.
    let binary = Path::new(env!("CARGO_BIN_EXE_omaddons"));
    let mut cmd = desk.command(binary, &["omarchy", "install", "--json"], &[]);
    cmd.env_remove(common::omarchy::WIDGET_REPO_ENV);
    let out = cmd.output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        desk.calls(),
        ["omarchy-plugin-add https://github.com/a44-io/omaddons-omarchy.git --yes"]
    );
    assert!(!desk.plugin().exists());
}

#[test]
fn a_second_install_is_unchanged_and_writes_nothing() {
    let desk = Desk::new();
    desk.install();
    let before = tree(&desk.config_omarchy());
    let history = fs::read(desk.history()).unwrap();
    let bell = fs::read(desk.doorbell()).unwrap();
    let calls = desk.calls().len();
    let d = desk.install();
    assert_eq!(d["data"]["installed"], "unchanged", "{d}");
    assert_eq!(d["data"]["enabled"], "already");
    assert_eq!(tree(&desk.config_omarchy()), before);
    assert_eq!(fs::read(desk.history()).unwrap(), history);
    assert_eq!(fs::read(desk.doorbell()).unwrap(), bell);
    assert_eq!(desk.calls().len(), calls, "no tool runs");
}

#[test]
fn no_enable_leaves_the_bar_and_a_later_install_places_it() {
    let desk = Desk::new();
    let d = desk.run(&["omarchy", "install", "--no-enable"]).expect(0);
    assert_eq!(d["data"]["installed"], "fresh");
    assert_eq!(d["data"]["enabled"], "skipped");
    assert_eq!(d["data"]["plugin"]["enabled"], false);
    assert!(!desk.calls().iter().any(|c| c.starts_with("omarchy-bar")));
    // The same files, not on the bar yet: placed now, in the section asked for, with
    // nothing written and nothing recorded.
    let lines = desk.widget_lines().len();
    let d = desk
        .run(&["omarchy", "install", "--section", "left"])
        .expect(0);
    assert_eq!(d["data"]["installed"], "unchanged");
    assert_eq!(d["data"]["enabled"], "yes");
    assert_eq!(d["data"]["section"], "left");
    assert_eq!(d["data"]["plugin"]["section"], "left");
    assert_eq!(
        desk.calls().last().unwrap(),
        &format!("omarchy-bar put {ID} --section left")
    );
    assert_eq!(desk.widget_lines().len(), lines);
}

#[test]
fn a_placed_widget_stays_where_the_owner_put_it() {
    let desk = Desk::new();
    desk.placed("center");
    let d = desk
        .run(&["omarchy", "install", "--section", "left"])
        .expect(0);
    assert_eq!(d["data"]["installed"], "fresh");
    assert_eq!(d["data"]["enabled"], "already");
    assert_eq!(d["data"]["plugin"]["section"], "center");
    assert!(!desk.calls().iter().any(|c| c.starts_with("omarchy-bar")));
}

#[test]
fn a_shell_still_rescanning_is_asked_again() {
    let desk = Desk::new();
    desk.set("bar-mode", "unknown:2");
    let d = desk.install();
    assert_eq!(d["data"]["enabled"], "yes", "{d}");
    let puts = desk
        .calls()
        .iter()
        .filter(|c| c.starts_with("omarchy-bar"))
        .count();
    assert_eq!(puts, 3);
}

#[test]
fn a_shell_that_never_answers_is_unreachable_and_the_files_stay() {
    for (mode, puts) in [("not-running", 1), ("unknown:99", 5)] {
        let desk = Desk::new();
        desk.set("bar-mode", mode);
        let d = desk.install();
        assert_eq!(d["data"]["installed"], "fresh", "{mode}");
        assert_eq!(d["data"]["enabled"], "failed", "{mode}");
        assert_eq!(d["data"]["plugin"]["installed"], true);
        let warnings = d["warnings"].as_array().unwrap();
        assert_eq!(warnings.len(), 1, "{d}");
        assert_eq!(warnings[0]["code"], "omarchy_shell_unreachable");
        assert_eq!(
            warnings[0]["fix_argv"],
            serde_json::json!(["omaddons", "omarchy", "install"])
        );
        assert!(
            warnings[0]["message"]
                .as_str()
                .unwrap()
                .contains("run it from the desktop session")
        );
        let tried = desk
            .calls()
            .iter()
            .filter(|c| c.starts_with("omarchy-bar"))
            .count();
        assert_eq!(tried, puts, "{mode}");
    }
}

#[test]
fn without_omarchy_there_is_no_bar() {
    // No `<config-omarchy>`; then the config dir but no shell anywhere.
    let none = Desk::bare();
    let shell_missing = Desk::bare();
    fs::create_dir_all(shell_missing.config_omarchy()).unwrap();
    shell_missing.tool("omarchy-bar");
    for desk in [none, shell_missing] {
        let d = desk.run(&["omarchy", "install"]).expect(3);
        assert_eq!(d["error"]["code"], "not_found", "{d}");
        assert_eq!(d["error"]["details"]["reason"], "omarchy_missing");
        assert_eq!(d["error"]["fix_argv"], Value::Null);
        assert!(
            d["error"]["fix"]
                .as_str()
                .unwrap()
                .contains("Omarchy desktop")
        );
        assert!(!desk.plugins().exists());
        assert!(!desk.history().exists());
        assert_eq!(desk.calls(), Vec::<String>::new());
    }
    // `$OMARCHY_PATH/shell` counts as the shell, without `omarchy-shell` on PATH.
    let desk = Desk::bare();
    fs::create_dir_all(desk.config_omarchy()).unwrap();
    fs::create_dir_all(desk.root().join("omarchy/shell")).unwrap();
    desk.tool("omarchy-bar");
    desk.tool("omarchy-plugin-add");
    let d = desk.install();
    assert_eq!(d["data"]["installed"], "fresh");
}

/// Omarchy refusing or failing the add: `download_failed` with what it said, nothing
/// recorded, and nothing left; an older widget it was to replace is back where it was.
#[test]
fn a_failed_add_leaves_everything_as_it_was() {
    let desk = Desk::new();
    desk.set("add-mode", "fail");
    let d = desk.run(&["omarchy", "install"]).expect(1);
    assert_eq!(d["error"]["code"], "download_failed", "{d}");
    assert_eq!(d["error"]["details"]["reason"], "plugin_add_failed");
    assert!(
        d["error"]["details"]["said"]
            .as_str()
            .unwrap()
            .contains("failed to clone"),
        "{d}"
    );
    assert_eq!(
        d["error"]["fix_argv"],
        serde_json::json!(["omaddons", "omarchy", "install"])
    );
    assert!(!desk.plugin().exists());
    assert_eq!(desk.widget_lines().len(), 0);
    assert_eq!(desk.leftovers(), Vec::<String>::new());
    assert_eq!(desk.inflight_locks(), 0);

    // An older render, moved out for the add, goes back whole.
    desk.rendered();
    let before = tree(&desk.plugin());
    let d = desk.run(&["omarchy", "install"]).expect(1);
    assert_eq!(d["error"]["details"]["reason"], "plugin_add_failed");
    assert_eq!(tree(&desk.plugin()), before);
    assert_eq!(desk.widget_lines().len(), 0);
    assert_eq!(desk.leftovers(), Vec::<String>::new());
    assert_eq!(desk.inflight_locks(), 0);

    // Failing after the checkout landed (its rescan): added, with a warning.
    desk.set("add-mode", "late-fail");
    let d = desk.install();
    assert_eq!(d["data"]["installed"], "replaced", "{d}");
    assert_eq!(d["warnings"][0]["code"], "omarchy_shell_unreachable", "{d}");
    assert_eq!(tree(&desk.plugin()), desk.checkout());
    assert_eq!(desk.widget_lines().len(), 1);
    assert_eq!(desk.leftovers(), Vec::<String>::new());
}

/// A dry run says what it would do, and runs and writes nothing.
#[test]
fn a_dry_run_writes_nothing_under_plugins() {
    let desk = Desk::new();
    let d = desk.run(&["omarchy", "install", "--dry-run"]).expect(0);
    assert_eq!(d["data"]["dry_run"], true);
    assert_eq!(d["data"]["installed"], "fresh");
    assert_eq!(d["data"]["enabled"], "yes");
    assert_eq!(d["data"]["plugin"]["installed"], false);
    assert!(!desk.plugins().exists());
    assert!(!desk.history().exists());
    assert!(!desk.doorbell().exists());
    assert_eq!(desk.calls(), Vec::<String>::new());

    desk.rendered();
    let before = tree(&desk.config_omarchy());
    let d = desk.run(&["omarchy", "install", "--dry-run"]).expect(0);
    assert_eq!(d["data"]["installed"], "replaced");
    assert_eq!(tree(&desk.config_omarchy()), before);
    assert_eq!(desk.calls(), Vec::<String>::new());

    fs::remove_dir_all(desk.plugin()).unwrap();
    desk.install();
    let d = desk.run(&["omarchy", "install", "--dry-run"]).expect(0);
    assert_eq!(d["data"]["installed"], "unchanged");
    assert_eq!(d["data"]["enabled"], "already");
}

/// `--offline` refuses an install that would clone, and writes nothing; one that has the
/// checkout already needs no network and places it.
#[test]
fn offline_refuses_only_an_install_that_would_clone() {
    let desk = Desk::new();
    let d = desk.run(&["omarchy", "install", "--offline"]).expect(1);
    assert_eq!(d["error"]["code"], "offline", "{d}");
    assert_eq!(d["error"]["details"]["reason"], "plugin_add_offline");
    assert!(!desk.plugins().exists());
    assert_eq!(desk.calls(), Vec::<String>::new());
    desk.rendered();
    let before = tree(&desk.config_omarchy());
    let d = desk.run(&["omarchy", "install", "--offline"]).expect(1);
    assert_eq!(d["error"]["code"], "offline");
    assert_eq!(tree(&desk.config_omarchy()), before);

    fs::remove_dir_all(desk.plugin()).unwrap();
    desk.run(&["omarchy", "install", "--no-enable"]).expect(0);
    let d = desk.run(&["omarchy", "install", "--offline"]).expect(0);
    assert_eq!(d["data"]["installed"], "unchanged");
    assert_eq!(d["data"]["enabled"], "yes");
}

/// The widget's checkout is Omarchy's to update: an install beside it writes nothing
/// under it, whatever it holds, and starts no tool once it is on the bar.
#[test]
fn a_checkout_is_never_written() {
    let desk = Desk::new();
    desk.install();
    fs::write(desk.plugin().join("local-note.txt"), "mine\n").unwrap();
    fs::write(desk.plugin().join(".git/FETCH_HEAD"), "abc\n").unwrap();
    let before = tree(&desk.plugin());
    let calls = desk.calls().len();
    let d = desk.install();
    assert_eq!(d["data"]["installed"], "unchanged");
    assert_eq!(tree(&desk.plugin()), before);
    assert_eq!(desk.calls().len(), calls);
    assert_eq!(desk.widget_lines().len(), 1);

    // A developer's symlink in its place is left alone too.
    let desk = Desk::new();
    fs::create_dir_all(desk.plugins()).unwrap();
    std::os::unix::fs::symlink(desk.repo(), desk.plugin()).unwrap();
    let d = desk.install();
    assert_eq!(d["data"]["installed"], "unchanged");
    assert!(desk.plugin().is_symlink());
    assert!(
        !desk
            .calls()
            .iter()
            .any(|c| c.starts_with("omarchy-plugin-add"))
    );
}

/// A widget an older omaddons rendered is moved out, the checkout added in its place,
/// and the move recorded; it keeps the place the owner gave it on the bar.
#[test]
fn a_rendered_widget_is_replaced_by_the_checkout() {
    let desk = Desk::new();
    desk.rendered();
    desk.placed("left");
    let reason = "migrate the widget";
    let d = desk
        .run(&["omarchy", "install", "--reason", reason])
        .expect(0);
    assert_eq!(d["data"]["installed"], "replaced", "{d}");
    assert_eq!(d["data"]["enabled"], "already");
    assert_eq!(d["data"]["plugin"]["outdated"], false);
    assert_eq!(d["data"]["plugin"]["section"], "left");
    assert_eq!(tree(&desk.plugin()), desk.checkout());
    let lines = desk.widget_lines();
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["reason"], reason);
    assert_eq!(
        desk.calls(),
        [format!(
            "omarchy-plugin-add {} --yes",
            desk.repo().display()
        )]
    );
    assert_eq!(desk.leftovers(), Vec::<String>::new());
    assert_eq!(desk.inflight_locks(), 0);
}

/// What a run killed between moving an older widget out and pruning it leaves, the
/// moved-out folder of a dead operation, the next install prunes; a live one it leaves.
#[test]
fn a_dead_runs_moved_out_widget_is_reclaimed() {
    let desk = Desk::new();
    let dead = desk
        .plugins()
        .join(".a44.omaddons.old-01M8Q2ZZZZZZZZZZZZZZZZZZZZ");
    fs::create_dir_all(dead.join("r-0123456789abcdef")).unwrap();
    fs::write(dead.join("manifest.json"), "{}").unwrap();
    let stage = desk
        .plugins()
        .join(".a44.omaddons.stage-01M8Q2YYYYYYYYYYYYYYYYYYYY");
    fs::create_dir_all(&stage).unwrap();
    desk.install();
    assert_eq!(desk.leftovers(), Vec::<String>::new());
    assert_eq!(desk.inflight_locks(), 0);
}

/// A folder in the widget's place that is neither its checkout nor a render (files
/// copied by hand, another plugin's checkout) is refused, never moved.
#[test]
fn a_folder_omaddons_did_not_install_is_refused() {
    let desk = Desk::new();
    let copied = desk.planted(ID, ID);
    let before = tree(&copied);
    let d = desk.run(&["omarchy", "install"]).expect(4);
    assert_eq!(d["error"]["code"], "file_exists", "{d}");
    assert_eq!(d["error"]["details"]["reason"], "plugin_folder_foreign");
    assert_eq!(tree(&copied), before);
    assert_eq!(desk.calls(), Vec::<String>::new());
    assert_eq!(desk.widget_lines().len(), 0);

    let desk = Desk::new();
    let other = desk.planted(ID, "someone.else");
    fs::create_dir_all(other.join(".git")).unwrap();
    let d = desk.run(&["omarchy", "install"]).expect(4);
    assert_eq!(d["error"]["details"]["reason"], "plugin_folder_foreign");
    assert!(other.join("Panel.qml").is_file());
}

/// The marketplace's setup plugin (omad-zhiq): a git checkout with the widget's id and a
/// bar widget only, in the widget's folder, already on the bar. The install moves it out
/// for the widget's checkout, which keeps the place the owner gave the setup plugin.
#[test]
fn the_setup_plugin_is_replaced_in_place() {
    let desk = Desk::new();
    let setup = desk.planted(ID, ID);
    fs::write(
        setup.join("manifest.json"),
        format!(
            r#"{{"schemaVersion": 1, "id": "{ID}", "kinds": ["bar-widget"], "entryPoints": {{"barWidget": "Panel.qml"}}}}"#
        ),
    )
    .unwrap();
    fs::create_dir_all(setup.join(".git/objects")).unwrap();
    fs::write(setup.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
    desk.placed("left");
    let d = desk.install();
    assert_eq!(d["data"]["installed"], "replaced", "{d}");
    assert_eq!(d["data"]["enabled"], "already");
    assert_eq!(d["data"]["plugin"]["outdated"], false);
    assert_eq!(d["data"]["plugin"]["section"], "left");
    assert_eq!(tree(&desk.plugin()), desk.checkout());
    assert!(!desk.calls().iter().any(|c| c.starts_with("omarchy-bar")));
    assert_eq!(desk.widget_lines().len(), 1);
    assert_eq!(desk.leftovers(), Vec::<String>::new());
    assert_eq!(desk.inflight_locks(), 0);
}

/// A widget a build before omad-o8g4 left in `<plugins>/omaddons` claims the id, which
/// Omarchy's add refuses: the install moves it out first, so exactly one is left. Beside
/// the checkout, an install still moves it out (recorded), and leaves the checkout as it
/// is.
#[test]
fn an_install_moves_a_legacy_widget_out() {
    let desk = Desk::new();
    desk.planted("omaddons", ID);
    let d = desk.install();
    assert_eq!(d["data"]["installed"], "fresh", "{d}");
    assert!(!desk.legacy().exists());
    assert_eq!(tree(&desk.plugin()), desk.checkout());
    assert_eq!(desk.leftovers(), Vec::<String>::new());
    assert_eq!(desk.inflight_locks(), 0);

    desk.planted("omaddons", ID);
    let lines = desk.widget_lines().len();
    let d = desk.install();
    assert_eq!(d["data"]["installed"], "unchanged", "{d}");
    assert!(!desk.legacy().exists());
    assert_eq!(tree(&desk.plugin()), desk.checkout());
    assert_eq!(desk.widget_lines().len(), lines + 1);
    assert_eq!(desk.leftovers(), Vec::<String>::new());
    assert_eq!(desk.inflight_locks(), 0);
}

/// `<plugins>/omaddons` holding another plugin, or a developer's symlink, is not a
/// legacy widget: the install leaves it alone. A symlink there that names the widget's
/// id holds the id, so Omarchy refuses the add, as it would anyone's.
#[test]
fn a_legacy_folder_that_is_not_the_widget_is_left_alone() {
    let desk = Desk::new();
    desk.planted("omaddons", "someone.else");
    desk.install();
    assert_eq!(desk.leftovers(), vec!["omaddons".to_owned()]);
    assert!(desk.legacy().join("Panel.qml").is_file());

    let desk = Desk::new();
    let checkout = desk.root().join("checkout");
    fs::create_dir_all(&checkout).unwrap();
    fs::write(
        checkout.join("manifest.json"),
        format!(r#"{{"schemaVersion": 1, "id": "{ID}"}}"#),
    )
    .unwrap();
    fs::create_dir_all(desk.plugins()).unwrap();
    std::os::unix::fs::symlink(&checkout, desk.legacy()).unwrap();
    let d = desk.run(&["omarchy", "install"]).expect(1);
    assert_eq!(d["error"]["details"]["reason"], "plugin_add_failed", "{d}");
    assert!(
        d["error"]["message"]
            .as_str()
            .unwrap()
            .contains("is already used by"),
        "{d}"
    );
    assert!(desk.legacy().is_symlink());
    assert!(checkout.join("manifest.json").is_file());
    assert!(!desk.plugin().exists());
}

/// `omarchy remove` takes away a widget an older build left in `<plugins>/omaddons`,
/// alone or beside the current one.
#[test]
fn remove_takes_a_legacy_widget_away() {
    let desk = Desk::new();
    desk.planted("omaddons", ID);
    let d = desk.run(&["omarchy", "remove"]).expect(0);
    assert_eq!(d["data"]["removed"], true, "{d}");
    assert!(!desk.legacy().exists());
    assert_eq!(desk.leftovers(), Vec::<String>::new());
    assert_eq!(desk.inflight_locks(), 0);

    desk.install();
    desk.planted("omaddons", ID);
    let d = desk.run(&["omarchy", "remove"]).expect(0);
    assert_eq!(d["data"]["removed"], true, "{d}");
    assert!(!desk.plugin().exists() && !desk.legacy().exists());
    assert_eq!(desk.leftovers(), Vec::<String>::new());
}

#[test]
fn the_human_output_names_the_widget_and_how_to_reinstall() {
    let desk = Desk::new();
    let binary = Path::new(env!("CARGO_BIN_EXE_omaddons"));
    let out = desk
        .command(binary, &["omarchy", "install"], &[])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    let text = String::from_utf8(out.stdout).unwrap();
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert_private_all(&text, &stderr);
    assert!(
        text.contains(&format!("installed the omaddons widget ({ID} ")),
        "{text}"
    );
    assert!(
        text.contains(&format!("\n  path: {}\n", desk.plugin().display())),
        "{text}"
    );
    assert!(
        text.contains("\n  bar: added to the right section\n"),
        "{text}"
    );
    assert!(text.contains("→ omaddons omarchy install"), "{text}");
    // What to do next (1n2p.35): where it is, how to move it, a key for it.
    let next = text.split("\nnext:\n").nth(1).unwrap_or_default();
    assert_eq!(
        next,
        format!(
            "  find it: the gamepad in the bar's right section; a click opens its panel\n  \
             move it: omarchy bar move {ID} --section <left|center|right>\n  \
             a key for it, in ~/.config/hypr/bindings.lua: o.bind(\"SUPER + ALT + W\", \"WoW addons\", {{ panel = \"{ID}\" }})\n"
        ),
        "{text}"
    );
    // A dry run, or a widget left off the bar, suggests nothing.
    let dry = desk
        .command(binary, &["omarchy", "install", "--dry-run"], &[])
        .output()
        .unwrap();
    assert!(!String::from_utf8(dry.stdout).unwrap().contains("\nnext:\n"));
}

// ---------------------------------------------------------------------------------------
// `omarchy status`.

/// The recommended actions of a `status` envelope.
fn actions(d: &Value) -> Vec<String> {
    d["commands"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            assert_eq!(
                c["argv"],
                serde_json::json!(["omaddons", "omarchy", "install"])
            );
            c["action"].as_str().unwrap().to_owned()
        })
        .collect()
}

#[test]
fn status_reports_omarchy_and_the_installed_widget() {
    let desk = Desk::new();
    let d = desk.run(&["omarchy"]).expect(0);
    assert_eq!(d["command"], "omarchy status");
    let data = &d["data"];
    assert_eq!(
        data["omarchy"],
        serde_json::json!({"present": true, "shell_running": true, "validator": true})
    );
    assert_eq!(data["plugin"]["installed"], false);
    assert_eq!(data["plugin"]["version"], Value::Null);
    assert_eq!(data["validation"], Value::Null);
    assert_eq!(actions(&d), Vec::<String>::new());

    desk.install();
    let history = fs::read(desk.history()).unwrap();
    let bell = fs::read(desk.doorbell()).unwrap();
    let before = tree(&desk.config_omarchy());
    let calls = desk.calls().len();
    let d = desk.run(&["omarchy", "status"]).expect(0);
    let p = &d["data"]["plugin"];
    assert_eq!(p["installed"], true);
    assert_eq!(p["path"], desk.plugin().display().to_string());
    assert_eq!(p["id"], ID);
    assert_eq!(p["version"], repo_version());
    assert_eq!(
        p["binary"],
        Value::Null,
        "the checkout finds omaddons itself"
    );
    assert_eq!(p["outdated"], false);
    assert_eq!(p["enabled"], true);
    assert_eq!(p["section"], "right");
    assert_eq!(
        d["data"]["validation"],
        serde_json::json!({"ok": true, "errors": []})
    );
    assert_eq!(actions(&d), Vec::<String>::new());
    // A read: nothing written.
    assert_eq!(fs::read(desk.history()).unwrap(), history);
    assert_eq!(fs::read(desk.doorbell()).unwrap(), bell);
    assert_eq!(tree(&desk.config_omarchy()), before);
    assert_eq!(
        desk.calls()[calls..],
        ["omarchy-shell shell ping"],
        "the ping alone"
    );

    // Off the bar: enable it.
    fs::write(desk.config_omarchy().join("shell.json"), "{}").unwrap();
    let d = desk.run(&["omarchy"]).expect(0);
    assert_eq!(d["data"]["plugin"]["enabled"], false);
    assert_eq!(d["data"]["plugin"]["section"], Value::Null);
    assert_eq!(actions(&d), ["enable_widget"]);

    // A widget an older omaddons rendered: outdated, and the install replaces it.
    fs::remove_dir_all(desk.plugin()).unwrap();
    desk.rendered();
    let d = desk.run(&["omarchy"]).expect(0);
    assert_eq!(d["data"]["plugin"]["outdated"], true);
    assert_eq!(d["data"]["plugin"]["context_matches"], true);
    assert_eq!(
        d["data"]["plugin"]["binary"],
        "/opt/omaddons-0.2.1/omaddons"
    );
    assert_eq!(actions(&d), ["reinstall_widget"]);
    let d = desk.install();
    assert_eq!(d["data"]["installed"], "replaced");
    let d = desk.run(&["omarchy"]).expect(0);
    assert_eq!(d["data"]["plugin"]["outdated"], false);

    // An installed widget that does not validate.
    fs::write(
        desk.plugin().join("manifest.json"),
        r#"{"schemaVersion": "1", "id": "a44.omaddons"}"#,
    )
    .unwrap();
    let d = desk.run(&["omarchy"]).expect(0);
    assert_eq!(d["data"]["validation"]["ok"], false);
    assert!(
        !d["data"]["validation"]["errors"]
            .as_array()
            .unwrap()
            .is_empty()
    );

    // Without Omarchy, status still answers.
    let bare = Desk::bare();
    let d = bare.run(&["omarchy"]).expect(0);
    assert_eq!(
        d["data"]["omarchy"],
        serde_json::json!({"present": false, "shell_running": false, "validator": false})
    );
}

/// Only `ok` from `omarchy-shell shell ping` within 2 s is a running shell: a silent
/// exit 0 (what `-q` gives) or a hang is not.
#[test]
fn the_shell_is_running_only_when_ping_answers_ok() {
    let desk = Desk::new();
    let running =
        |desk: &Desk| desk.run(&["omarchy"]).expect(0)["data"]["omarchy"]["shell_running"].clone();
    assert_eq!(running(&desk), true);
    assert!(
        desk.calls()
            .contains(&"omarchy-shell shell ping".to_owned())
    );
    desk.set("shell-mode", "silent");
    assert_eq!(running(&desk), false);
    desk.set("shell-mode", "hang");
    let started = std::time::Instant::now();
    assert_eq!(running(&desk), false);
    let took = started.elapsed();
    assert!(took >= std::time::Duration::from_secs(2), "{took:?}");
    assert!(took < std::time::Duration::from_secs(5), "{took:?}");
}

/// A tool that answers and exits while a process it started keeps its output pipe open
/// (a restarted shell) is done when it exits: its answer counts, and omaddons never waits
/// on the other process.
#[test]
fn a_tool_that_leaves_a_process_holding_its_output_still_answers() {
    let desk = Desk::new();
    desk.set("shell-mode", "daemon");
    let started = std::time::Instant::now();
    let shell = desk.run(&["omarchy"]).expect(0)["data"]["omarchy"]["shell_running"].clone();
    let took = started.elapsed();
    let pid = fs::read_to_string(desk.root().join("orphan.pid")).unwrap();
    // The test's own cleanup: nothing stops a process a tool that finished started.
    let _ = std::process::Command::new("kill").arg(pid.trim()).status();
    assert_eq!(shell, true, "its `ok` counts");
    assert!(took < std::time::Duration::from_secs(10), "{took:?}");
}

/// A tool that times out is stopped with every process it started (its process group):
/// a child it left running in the background is gone once omaddons returns.
#[test]
fn a_timed_out_tool_leaves_no_process_of_its_own_behind() {
    let desk = Desk::new();
    desk.set("shell-mode", "orphan");
    let shell = desk.run(&["omarchy"]).expect(0)["data"]["omarchy"]["shell_running"].clone();
    assert_eq!(shell, false);
    let pid = fs::read_to_string(desk.root().join("orphan.pid")).unwrap();
    let stat = Path::new("/proc").join(pid.trim()).join("stat");
    // Gone, or a zombie its new parent has not reaped yet.
    let alive = || {
        fs::read_to_string(&stat).is_ok_and(|s| {
            s.rsplit_once(") ")
                .is_some_and(|(_, rest)| !rest.starts_with('Z'))
        })
    };
    let started = std::time::Instant::now();
    while alive() && started.elapsed() < std::time::Duration::from_secs(2) {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(
        !alive(),
        "the tool's background child {} still runs",
        pid.trim()
    );
}

/// A widget an older omaddons rendered for another state is noted, with nothing
/// recommended; one rendered for this state is outdated, with its reinstall.
#[test]
fn status_and_its_human_output_note_another_state() {
    let desk = Desk::new();
    desk.rendered();
    let elsewhere = desk.root().join("other-state");
    fs::create_dir_all(&elsewhere).unwrap();
    let binary = Path::new(env!("CARGO_BIN_EXE_omaddons"));
    let out = desk
        .command(
            binary,
            &["omarchy"],
            &[("XDG_STATE_HOME", elsewhere.to_str().unwrap())],
        )
        .output()
        .unwrap();
    let text = String::from_utf8(out.stdout).unwrap();
    assert_private_all(&text, &String::from_utf8(out.stderr).unwrap());
    assert!(
        text.contains(&format!("\n  path: {}\n", desk.plugin().display())),
        "{text}"
    );
    assert!(
        text.contains("\n  note: it serves another omaddons state"),
        "{text}"
    );
    assert!(
        text.contains("Omarchy: present · shell running · "),
        "{text}"
    );
    assert!(
        !text.contains("omaddons omarchy install"),
        "nothing is recommended for another state's widget: {text}"
    );

    // For this state: no such note, outdated, and its reinstall is recommended.
    let text = human(&desk, &["omarchy"], &[]);
    assert!(!text.contains("serves another omaddons state"), "{text}");
    assert!(
        text.contains(
            "\n  outdated: an older omaddons rendered it; omarchy install replaces it with the widget's checkout\n"
        ),
        "{text}"
    );
    assert!(text.contains("\n  → omaddons omarchy install\n"), "{text}");
}

// ---------------------------------------------------------------------------------------
// `status`'s widget block (E4).

/// The warning codes of an envelope.
fn warning_codes(d: &Value) -> Vec<String> {
    d["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["code"].as_str().unwrap().to_owned())
        .collect()
}

/// `omaddons <args>`'s human output on `desk`, with `extra`.
fn human(desk: &Desk, args: &[&str], extra: &[(&str, &str)]) -> String {
    let binary = Path::new(env!("CARGO_BIN_EXE_omaddons"));
    let out = desk.command(binary, args, extra).output().unwrap();
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).unwrap();
    assert_private_all(&text, &String::from_utf8(out.stderr).unwrap());
    text
}

/// `status` reports the bar widget: `null` without Omarchy, then not installed, current,
/// off the bar, outdated (with the warning, the machine's issue, the recommendation and
/// `summary.widget_outdated`), seen from another omaddons state (a note, nothing
/// recommended), and without its `Build.js`. It runs no Omarchy tool and writes nothing.
#[test]
fn status_reports_the_bar_widget() {
    let bare = Desk::bare();
    let d = bare.run(&["status"]).expect(0);
    assert_eq!(d["data"]["widget"], Value::Null);
    assert_eq!(d["data"]["issues"], serde_json::json!([]));
    assert!(!warning_codes(&d).contains(&"widget_outdated".to_owned()));

    let desk = Desk::new();
    let widget = |d: &Value| d["data"]["widget"].clone();
    let binary = this_binary().display().to_string();
    let doorbell = desk.env.cache().join("omaddons").display().to_string();
    let d = desk.run(&["status"]).expect(0);
    assert_eq!(
        widget(&d),
        serde_json::json!({"omarchy": true, "installed": false, "enabled": false, "version": null,
            "outdated": false, "context_matches": null, "binary": binary, "doorbell_dir": doorbell})
    );
    assert!(warning_codes(&d).is_empty(), "{d:#}");

    desk.install();
    let history = fs::read(desk.history()).unwrap();
    let bell = fs::read(desk.doorbell()).unwrap();
    let before = tree(&desk.config_omarchy());
    let calls = desk.calls().len();
    for args in [&["status"][..], &["status", "--cached"]] {
        let d = desk.run(args).expect(0);
        // The checkout's files say nothing of whose state it serves (null): it runs in its
        // desktop session (omad-widget-plugin-3bcbh.10 asks it).
        assert_eq!(
            widget(&d),
            serde_json::json!({"omarchy": true, "installed": true, "enabled": true,
                "version": repo_version(), "outdated": false, "context_matches": null,
                "binary": binary, "doorbell_dir": doorbell})
        );
        assert_eq!(d["data"]["summary"]["widget_outdated"], false);
        assert_eq!(d["data"]["issues"], serde_json::json!([]));
        assert!(warning_codes(&d).is_empty(), "{d:#}");
    }
    assert_eq!(desk.calls().len(), calls, "status runs no Omarchy tool");
    assert_eq!(fs::read(desk.history()).unwrap(), history);
    assert_eq!(fs::read(desk.doorbell()).unwrap(), bell);
    assert_eq!(tree(&desk.config_omarchy()), before);
    assert!(
        !human(&desk, &["status"], &[]).contains("bar widget"),
        "a current widget goes unmentioned"
    );

    fs::write(desk.config_omarchy().join("shell.json"), "{}").unwrap();
    assert_eq!(widget(&desk.run(&["status"]).expect(0))["enabled"], false);

    // A widget an older omaddons rendered for this state: outdated, and the triage says
    // so.
    fs::remove_dir_all(desk.plugin()).unwrap();
    desk.rendered();
    let d = desk.run(&["status", "--cached"]).expect(0);
    assert_eq!(widget(&d)["outdated"], true);
    assert_eq!(warning_codes(&d), ["widget_outdated"]);
    assert_eq!(d["data"]["summary"]["widget_outdated"], true);
    assert_eq!(d["data"]["summary"]["issues"]["p3"], 1);
    assert_eq!(d["data"]["summary"]["state"], "idle", "a P3 is not urgent");
    assert_eq!(
        d["data"]["issues"][0]["id"],
        "fm-plugins-widget-version-drift"
    );
    assert_eq!(actions(&d), ["reinstall_widget"]);
    assert!(
        human(&desk, &["status"], &[])
            .contains("bar widget: from another omaddons build → omaddons omarchy install")
    );

    // From another omaddons state: the facts and a note, but no reinstall from here,
    // which would point the widget at this state.
    let elsewhere = desk.root().join("other-state");
    fs::create_dir_all(&elsewhere).unwrap();
    let other = [("XDG_STATE_HOME", elsewhere.to_str().unwrap())];
    let d = desk.run_with(&["status"], &other).expect(0);
    assert_eq!(
        (&widget(&d)["outdated"], &widget(&d)["context_matches"]),
        (&Value::Bool(true), &Value::Bool(false))
    );
    assert!(warning_codes(&d).is_empty(), "{d:#}");
    assert_eq!(d["data"]["issues"], serde_json::json!([]));
    assert_eq!(d["data"]["summary"]["widget_outdated"], false);
    assert_eq!(actions(&d), Vec::<String>::new());
    let note =
        "bar widget: serves another omaddons state (installed under other HOME or XDG directories)";
    let text = human(&desk, &["status"], &other);
    assert!(
        text.contains(&format!("{note}, from another omaddons build")),
        "{text}"
    );
    assert!(!text.contains("→ omaddons omarchy install"), "{text}");

    // Replaced by the checkout: current, from either state, and nothing noted.
    desk.install();
    for extra in [&[][..], &other] {
        let d = desk.run_with(&["status"], extra).expect(0);
        assert_eq!(
            (&widget(&d)["outdated"], &widget(&d)["context_matches"]),
            (&Value::Bool(false), &Value::Null)
        );
        assert!(warning_codes(&d).is_empty(), "{d:#}");
        assert_eq!(actions(&d), Vec::<String>::new());
    }
    let text = human(&desk, &["status"], &other);
    assert!(
        !text.contains(note) && !text.contains("from another omaddons build"),
        "{text}"
    );
}

/// `status` tells the widget the path to run omaddons by and the directory the doorbell
/// rings in (omad-widget-plugin-3bcbh.2), with or without `--cached`: mise's `latest`
/// link for the version mise links there, any other binary (an older mise version
/// included) by its canonical path, and the cache directory as this run resolves it, a
/// relative `XDG_CACHE_HOME` ignored as everywhere. It starts no Omarchy tool for them.
#[test]
fn status_names_the_binary_and_the_doorbell_for_the_widget() {
    use std::os::unix::fs::symlink;
    let desk = Desk::new();
    desk.run(&["status"]).expect(0);
    let widget = |d: &Value| d["data"]["widget"].clone();
    let doorbell = desk.env.cache().join("omaddons").display().to_string();

    // A mise layout in the target's tmp dir, beside the binary, so a hard link usually
    // does; a copy where it cannot.
    let mise = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let tool = fs::canonicalize(mise.path())
        .unwrap()
        .join("installs/github-a44-io-omaddons");
    let place = |version: &str| -> PathBuf {
        let at = tool.join(version).join("omaddons");
        fs::create_dir_all(at.parent().unwrap()).unwrap();
        if fs::hard_link(this_binary(), &at).is_err() {
            fs::copy(this_binary(), &at).unwrap();
        }
        at
    };
    let newest = place("0.3.0");
    let older = place("0.2.1");
    symlink("0.3.0", tool.join("latest")).unwrap();

    let calls = desk.calls().len();
    for args in [&["status"][..], &["status", "--cached"]] {
        for (binary, expected) in [
            (this_binary(), this_binary()),
            (newest.clone(), tool.join("latest/omaddons")),
            (older.clone(), older.clone()),
        ] {
            let d = desk.run_binary(&binary, args, &[]).expect(0);
            assert_eq!(
                (
                    args,
                    binary.display().to_string(),
                    widget(&d)["binary"].clone()
                ),
                (
                    args,
                    binary.display().to_string(),
                    Value::from(expected.display().to_string())
                )
            );
            assert_eq!(widget(&d)["doorbell_dir"], doorbell.as_str());
        }
    }
    assert_eq!(desk.calls().len(), calls, "status runs no Omarchy tool");

    // A relative XDG_CACHE_HOME is not a cache directory: the default one, as every
    // command resolves it.
    let d = desk
        .run_with(&["status"], &[("XDG_CACHE_HOME", "relative/cache")])
        .expect(0);
    assert_eq!(
        widget(&d)["doorbell_dir"],
        desk.env
            .home()
            .join(".cache/omaddons")
            .display()
            .to_string()
    );

    // Without Omarchy there is no widget to tell.
    assert_eq!(
        Desk::bare().run(&["status"]).expect(0)["data"]["widget"],
        Value::Null
    );
}

// ---------------------------------------------------------------------------------------
// `omarchy remove`.

#[test]
fn remove_takes_it_off_the_bar_deletes_it_records_and_rings() {
    let desk = Desk::new();
    desk.install();
    let calls = desk.calls().len();
    let d = desk.run(&["omarchy", "remove"]).expect(0);
    assert_eq!(
        d["data"],
        serde_json::json!({"dry_run": false, "removed": true, "disabled": true, "settings_dropped": false})
    );
    assert_eq!(d["warnings"], serde_json::json!([]));
    assert_eq!(
        desk.calls()[calls..],
        [format!("omarchy-plugin-disable {ID}")]
    );
    // Nothing of the widget's is left in Omarchy's directory, hidden or not.
    assert_eq!(
        fs::read_dir(desk.plugins()).unwrap().count(),
        0,
        "{:?}",
        desk.leftovers()
    );
    assert_eq!(desk.inflight_locks(), 0);
    let lines = desk.widget_lines();
    assert_eq!(lines.len(), 2);
    assert_ne!(lines[0]["tx"], lines[1]["tx"]);
    let bell: Value = serde_json::from_slice(&fs::read(desk.doorbell()).unwrap()).unwrap();
    assert_eq!(bell["command"], "omarchy remove");
    let d = desk.run(&["omarchy"]).expect(0);
    assert_eq!(d["data"]["plugin"]["installed"], false);
    assert_eq!(d["data"]["plugin"]["enabled"], false);

    // Removing again: nothing to remove.
    let d = desk.run(&["omarchy", "remove"]).expect(3);
    assert_eq!(d["error"]["code"], "not_found");
    assert_eq!(d["error"]["details"]["reason"], "widget_not_installed");
    assert_eq!(
        d["error"]["fix_argv"],
        serde_json::json!(["omaddons", "omarchy", "install"])
    );
}

/// Omarchy drops the widget's inline bar settings with its entry: said in data and in
/// the human output.
#[test]
fn remove_says_when_omarchy_drops_the_bar_settings() {
    let desk = Desk::new();
    fs::write(
        desk.config_omarchy().join("shell.json"),
        format!(r#"{{"bar":{{"layout":{{"right":[{{"id":"{ID}","binary":"/opt/omaddons"}}]}}}}}}"#),
    )
    .unwrap();
    assert_eq!(desk.install()["data"]["enabled"], "already");
    let d = desk.run(&["omarchy", "remove", "--dry-run"]).expect(0);
    assert_eq!(d["data"]["settings_dropped"], true);
    let binary = Path::new(env!("CARGO_BIN_EXE_omaddons"));
    let out = desk
        .command(binary, &["omarchy", "remove"], &[])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.contains("removed the omaddons widget"), "{text}");
    assert!(
        text.contains("the widget's bar settings were removed by Omarchy with it"),
        "{text}"
    );
}

/// The disable is best effort: the files go anyway, and a warning says what stays.
/// Without `omarchy-plugin-disable`, the shell's own `setPluginEnabled` does it; a
/// widget on no section of the bar needs neither.
#[test]
fn remove_without_a_disable_still_removes_the_files() {
    let desk = Desk::new();
    desk.install();
    desk.set("disable-mode", "fail");
    let d = desk.run(&["omarchy", "remove"]).expect(0);
    assert_eq!(d["data"]["removed"], true);
    assert_eq!(d["data"]["disabled"], false);
    assert_eq!(d["data"]["settings_dropped"], false);
    assert_eq!(d["warnings"][0]["code"], "omarchy_shell_unreachable");
    assert_eq!(d["warnings"][0]["fix_argv"], Value::Null);
    assert!(
        d["warnings"][0]["fix"]
            .as_str()
            .unwrap()
            .contains(&format!("omarchy plugin disable {ID}"))
    );
    assert!(!desk.plugin().exists());

    let desk = Desk::new();
    desk.install();
    fs::remove_file(desk.root().join("bin/omarchy-plugin-disable")).unwrap();
    let d = desk.run(&["omarchy", "remove"]).expect(0);
    assert_eq!(d["data"]["disabled"], true);
    assert!(
        desk.calls()
            .contains(&format!("omarchy-shell shell setPluginEnabled {ID} false"))
    );

    let desk = Desk::new();
    desk.run(&["omarchy", "install", "--no-enable"]).expect(0);
    let calls = desk.calls().len();
    let d = desk.run(&["omarchy", "remove"]).expect(0);
    assert_eq!(d["data"]["disabled"], true);
    assert_eq!(desk.calls().len(), calls, "nothing to take off the bar");
}

#[test]
fn remove_dry_run_changes_nothing() {
    let desk = Desk::new();
    desk.install();
    let before = tree(&desk.config_omarchy());
    let history = fs::read(desk.history()).unwrap();
    let bell = fs::read(desk.doorbell()).unwrap();
    let calls = desk.calls().len();
    let d = desk.run(&["omarchy", "remove", "--dry-run"]).expect(0);
    assert_eq!(
        d["data"],
        serde_json::json!({"dry_run": true, "removed": true, "disabled": true, "settings_dropped": false})
    );
    assert_eq!(tree(&desk.config_omarchy()), before);
    assert_eq!(fs::read(desk.history()).unwrap(), history);
    assert_eq!(fs::read(desk.doorbell()).unwrap(), bell);
    assert_eq!(desk.calls().len(), calls, "no disable call");
}

/// Two removes: the first, paused after its disable, finds the widget gone once the
/// second has removed it.
#[test]
fn of_two_removes_one_removes_and_the_other_finds_nothing() {
    let desk = Desk::new();
    desk.install();
    let pause = Pause::new(desk.root());
    let binary = Path::new(env!("CARGO_BIN_EXE_omaddons"));
    let first = desk
        .command(
            binary,
            &["omarchy", "remove", "--json"],
            &[
                ("OMADDONS_FAILPOINTS", "omarchy.removing=pause"),
                pause.env(),
            ],
        )
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    pause.reached("omarchy.removing");
    desk.run(&["omarchy", "remove"]).expect(0);
    pause.resume("omarchy.removing");
    let d = wait(first, &["omarchy", "remove"]).expect(3);
    assert_eq!(d["error"]["code"], "not_found");
    assert_eq!(desk.widget_lines().len(), 2, "one install, one remove");
    assert_eq!(fs::read_dir(desk.plugins()).unwrap().count(), 0);
    assert_eq!(desk.inflight_locks(), 0);
}
