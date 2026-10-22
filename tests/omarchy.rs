//! `omaddons omarchy install` end to end (E2): a fake `<config-omarchy>` and stub
//! `omarchy-*` tools that log their argv and `OMARCHY_PATH`. omaddons runs with `PATH`
//! holding the stubs alone, so the real tools (whose `omarchy-bar put` would place the
//! widget on this machine's bar) are never reachable; the stubs use shell builtins only.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use common::env::TestEnv;
use common::home::{Run, wait};
use common::pause::Pause;
use common::privacy::assert_private_all;
use omaddons::omarchy::render;
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
        for tool in ["omarchy-shell", "omarchy-plugin-validate", "omarchy-bar"] {
            desk.tool(tool);
        }
        desk
    }

    /// No Omarchy at all: an empty stub dir and an `OMARCHY_PATH` without `shell/`.
    fn bare() -> Self {
        let env = TestEnv::new();
        fs::create_dir_all(env.root().join("bin")).unwrap();
        fs::create_dir_all(env.root().join("omarchy")).unwrap();
        Self { env }
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
        self.plugins().join("omaddons")
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

    /// A stub tool. `omarchy-bar` answers as `<root>/bar-mode` says (`ok`, `not-running`,
    /// `unknown:<n>` for n "not a known widget" answers first) and, placing, writes the
    /// section into `shell.json` as Omarchy's shell would; `omarchy-plugin-validate`
    /// refuses when `<root>/validate-mode` says `bad`.
    fn tool(&self, name: &str) {
        let root = self.root().display();
        let log = format!(
            r#"printf '%s %s\n' "${{0##*/}}" "$*" >> {root}/tools.log
printf 'OMARCHY_PATH=%s\n' "$OMARCHY_PATH" >> {root}/tools.log"#
        );
        let body = match name {
            "omarchy-bar" => format!(
                r#"mode=ok; [ -f {root}/bar-mode ] && read mode < {root}/bar-mode
case "$mode" in
  not-running) echo "omarchy-shell is not running" >&2; exit 0 ;;
  unknown:*)
    n=0; [ -f {root}/bar-count ] && read n < {root}/bar-count; n=$((n+1)); echo $n > {root}/bar-count
    if [ $n -le "${{mode#unknown:}}" ]; then echo "$2 is not a known widget" >&2; exit 1; fi ;;
esac
printf '{{"bar":{{"layout":{{"%s":[{{"id":"%s"}}]}}}}}}' "$4" "$2" > "$XDG_CONFIG_HOME/omarchy/shell.json"
echo "$2 is on the bar""#
            ),
            "omarchy-plugin-validate" => format!(
                r#"mode=ok; [ -f {root}/validate-mode ] && read mode < {root}/validate-mode
if [ "$mode" = bad ]; then echo "manifest.json: entryPoints.barWidget is missing" >&2; exit 1; fi"#
            ),
            _ => String::new(),
        };
        let path = self.root().join("bin").join(name);
        fs::write(&path, format!("#!/bin/sh\n{log}\n{body}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
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

    /// `omarchy install --json` in the background, armed with `extra`.
    fn spawn(&self, extra: &[(&str, &str)]) -> std::process::Child {
        let binary = Path::new(env!("CARGO_BIN_EXE_omaddons"));
        self.command(binary, &["omarchy", "install", "--json"], extra)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap()
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

    /// The `<plugins>` entries other than the widget and the prune trash.
    fn leftovers(&self) -> Vec<String> {
        let Ok(entries) = fs::read_dir(self.plugins()) else {
            return Vec::new();
        };
        let mut names: Vec<String> = entries
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .filter(|n| n != "omaddons" && n != ".omaddons.trash")
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

/// A `Build.js` line's value.
fn build_value(dir: &Path, prefix: &str) -> String {
    let build = tree(dir)
        .into_iter()
        .find(|(rel, _)| rel.ends_with("/Build.js"))
        .map(|(_, bytes)| String::from_utf8(bytes).unwrap())
        .unwrap();
    build
        .lines()
        .find_map(|l| l.strip_prefix(prefix))
        .unwrap()
        .trim_end_matches(';')
        .to_owned()
}

fn this_binary() -> PathBuf {
    fs::canonicalize(env!("CARGO_BIN_EXE_omaddons")).unwrap()
}

#[test]
fn a_fresh_install_validates_publishes_records_rings_and_places() {
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
    assert_eq!(plugin["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(plugin["binary"], this_binary().display().to_string());
    assert_eq!(plugin["outdated"], false);
    assert_eq!(plugin["enabled"], true);
    assert_eq!(plugin["section"], "right");
    assert_eq!(plugin["context_matches"], true);
    assert_eq!(d["warnings"], serde_json::json!([]));

    // The stage was validated (by its hidden name), then the shell rescanned, then put.
    let calls = desk.calls();
    assert_eq!(calls.len(), 3, "{calls:?}");
    let staged = calls[0].strip_prefix("omarchy-plugin-validate ").unwrap();
    let stage_name = Path::new(staged).file_name().unwrap().to_str().unwrap();
    assert!(stage_name.starts_with(".omaddons.stage-"), "{staged}");
    assert_eq!(Path::new(staged).parent().unwrap(), desk.plugins());
    assert_eq!(calls[1], "omarchy-shell -q shell rescanPlugins");
    assert_eq!(calls[2], format!("omarchy-bar put {ID} --section right"));

    let lines = desk.widget_lines();
    assert_eq!(lines.len(), 1);
    let op = stage_name.trim_start_matches(".omaddons.stage-");
    assert_eq!(lines[0]["tx"], op, "the operation that published");
    assert_eq!(lines[0]["target"], Value::Null);
    assert_eq!(lines[0]["ops"], serde_json::json!([]));
    let bell: Value = serde_json::from_slice(&fs::read(desk.doorbell()).unwrap()).unwrap();
    assert_eq!(bell["command"], "omarchy install");

    assert!(desk.plugin().join("manifest.json").is_file());
    assert_eq!(
        build_value(&desk.plugin(), "var BINARY = "),
        format!("\"{}\"", this_binary().display())
    );
    assert_eq!(desk.leftovers(), Vec::<String>::new());
    assert_eq!(desk.inflight_locks(), 0);
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
    let d = desk.install();
    assert_eq!(d["data"]["installed"], "fresh");
}

#[test]
fn an_invalid_render_is_internal_and_nothing_is_swapped() {
    let desk = Desk::new();
    let r = desk.run_with(
        &["omarchy", "install"],
        &[("OMADDONS_FAILPOINTS", "omarchy.invalid")],
    );
    let d = r.expect(1);
    assert_eq!(d["error"]["code"], "internal", "{d}");
    assert_eq!(d["error"]["details"]["reason"], "plugin_invalid");
    assert!(
        !d["error"]["details"]["problems"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(!desk.plugin().exists());
    assert_eq!(desk.leftovers(), Vec::<String>::new());
    assert_eq!(desk.widget_lines().len(), 0);
    assert_eq!(desk.inflight_locks(), 0);

    // Omarchy's own validator refusing is the same build bug, in its words.
    desk.set("validate-mode", "bad");
    let d = desk.run(&["omarchy", "install"]).expect(1);
    assert_eq!(d["error"]["details"]["reason"], "plugin_invalid");
    let problems = d["error"]["details"]["problems"].to_string();
    assert!(
        problems.contains("omarchy-plugin-validate: manifest.json"),
        "{problems}"
    );
    assert!(!desk.plugin().exists());
    assert_eq!(desk.leftovers(), Vec::<String>::new());

    // Without it, omaddons' own port is enough.
    fs::remove_file(desk.root().join("bin/omarchy-plugin-validate")).unwrap();
    assert_eq!(desk.install()["data"]["installed"], "fresh");
}

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
    // Only the read-only validator ran, on a scratch copy that is gone.
    let calls = desk.calls();
    assert_eq!(calls.len(), 1, "{calls:?}");
    let scratch = calls[0].strip_prefix("omarchy-plugin-validate ").unwrap();
    assert!(
        scratch.starts_with(&desk.env.cache().display().to_string()),
        "{scratch}"
    );
    assert!(!Path::new(scratch).exists());

    desk.install();
    let d = desk.run(&["omarchy", "install", "--dry-run"]).expect(0);
    assert_eq!(d["data"]["installed"], "unchanged");
    assert_eq!(d["data"]["enabled"], "already");
}

/// A moved binary restamps `Build.js` through an exchange; the code hash excludes the
/// binary, so `status` would not call the installed widget outdated before it. Run as
/// the widget's self-reinstall runs it, the history line says so.
#[test]
fn a_moved_binary_is_replaced_and_its_code_hash_is_unchanged() {
    let desk = Desk::new();
    desk.install();
    let moved = desk.root().join("moved/omaddons");
    fs::create_dir_all(moved.parent().unwrap()).unwrap();
    fs::copy(this_binary(), &moved).unwrap();

    let context = render::installed_context(&desk.plugin()).unwrap();
    let installed = render::installed_sha256(&desk.plugin()).unwrap();
    let rerendered = render::render(&moved, &context, ID).unwrap();
    assert_eq!(rerendered.content_sha256, installed);

    let reason = "reinstall from the bar widget";
    let args = [
        "omarchy", "install", "--actor", "widget", "--reason", reason,
    ];
    let d = desk.run_binary(&moved, &args, &[]).expect(0);
    assert_eq!(d["data"]["installed"], "replaced", "{d}");
    assert_eq!(d["data"]["enabled"], "already");
    assert_eq!(d["data"]["plugin"]["binary"], moved.display().to_string());
    assert_eq!(
        build_value(&desk.plugin(), "var BINARY = "),
        format!("\"{}\"", moved.display())
    );
    assert_eq!(render::installed_sha256(&desk.plugin()).unwrap(), installed);
    let lines = desk.widget_lines();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[1]["actor"], "widget");
    assert_eq!(lines[1]["reason"], reason);
    let bell: Value = serde_json::from_slice(&fs::read(desk.doorbell()).unwrap()).unwrap();
    assert_eq!(bell["actor"], "widget");
    assert_eq!(desk.leftovers(), Vec::<String>::new());
    assert_eq!(desk.inflight_locks(), 0);
}

/// An installed widget someone changed: the next install replaces it with the render.
fn tampered(desk: &Desk) -> (BTreeMap<String, Vec<u8>>, BTreeMap<String, Vec<u8>>) {
    desk.install();
    let fresh = tree(&desk.plugin());
    let (model, _) = fresh
        .iter()
        .find(|(rel, _)| rel.ends_with("/Model.js"))
        .unwrap();
    let path = desk.plugin().join(model);
    let mut bytes = fs::read(&path).unwrap();
    bytes.extend_from_slice(b"\n// changed by hand\n");
    fs::write(&path, bytes).unwrap();
    (tree(&desk.plugin()), fresh)
}

fn stages(desk: &Desk) -> Vec<String> {
    desk.leftovers()
        .into_iter()
        .filter(|n| n.starts_with(".omaddons.stage-"))
        .collect()
}

/// On the exchange path a crash leaves the old or the new widget, whole, and at most a
/// hidden stage; the next install finishes.
#[test]
fn a_crash_around_the_exchange_leaves_one_whole_widget() {
    // Before the publishing rename: the old widget, and the new render staged.
    let desk = Desk::new();
    let (old, new) = tampered(&desk);
    let r = desk.run_with(
        &["omarchy", "install"],
        &[("OMADDONS_FAILPOINTS", "omarchy.swap")],
    );
    assert_eq!(r.code, -1, "{}", r.context());
    assert_eq!(tree(&desk.plugin()), old);
    let left = stages(&desk);
    assert_eq!(left.len(), 1);
    assert_eq!(tree(&desk.plugins().join(&left[0])), new);
    // The next install reclaims the dead stage and publishes.
    let d = desk.install();
    assert_eq!(d["data"]["installed"], "replaced");
    assert_eq!(tree(&desk.plugin()), new);
    assert_eq!(desk.leftovers(), Vec::<String>::new());
    assert_eq!(desk.inflight_locks(), 0);

    // Right after it, before history: the new widget, the old one at the stage's name.
    let desk = Desk::new();
    let (old, new) = tampered(&desk);
    let lines = desk.widget_lines().len();
    let r = desk.run_with(
        &["omarchy", "install"],
        &[("OMADDONS_FAILPOINTS", "fsx.append_durable.before")],
    );
    assert_eq!(r.code, -1, "{}", r.context());
    assert_eq!(tree(&desk.plugin()), new);
    let left = stages(&desk);
    assert_eq!(left.len(), 1);
    assert_eq!(tree(&desk.plugins().join(&left[0])), old);
    assert_eq!(desk.widget_lines().len(), lines);
    // The widget is current: nothing to do, the hidden stage waits for the next publish.
    assert_eq!(desk.install()["data"]["installed"], "unchanged");
}

/// Without `RENAME_EXCHANGE` the swap is two renames: a crash between them leaves no
/// widget and a hidden parked one, and the next install finishes as a first install.
#[test]
fn without_exchange_a_crash_between_the_renames_is_finished_by_the_next_install() {
    let desk = Desk::new();
    let (old, new) = tampered(&desk);
    let r = desk.run_with(
        &["omarchy", "install"],
        &[("OMADDONS_FAILPOINTS", "fsx.renameat2.einval,omarchy.parked")],
    );
    assert_eq!(r.code, -1, "{}", r.context());
    assert!(!desk.plugin().exists());
    let parked: Vec<String> = desk
        .leftovers()
        .into_iter()
        .filter(|n| n.starts_with(".omaddons.old-"))
        .collect();
    assert_eq!(parked.len(), 1, "{:?}", desk.leftovers());
    assert_eq!(tree(&desk.plugins().join(&parked[0])), old);
    assert_eq!(stages(&desk).len(), 1);

    let d = desk.install();
    assert_eq!(d["data"]["installed"], "fresh", "{d}");
    assert_eq!(tree(&desk.plugin()), new);
    assert_eq!(desk.leftovers(), Vec::<String>::new());
    assert_eq!(desk.inflight_locks(), 0);

    // Uninterrupted, the fallback replaces and says it used two renames.
    let (_, new) = tampered(&desk);
    let d = desk
        .run_with(
            &["omarchy", "install"],
            &[("OMADDONS_FAILPOINTS", "fsx.renameat2.einval")],
        )
        .expect(0);
    assert_eq!(d["data"]["installed"], "replaced", "{d}");
    assert_eq!(d["warnings"][0]["code"], "atomic_swap_unavailable");
    assert_eq!(tree(&desk.plugin()), new);
    assert_eq!(desk.leftovers(), Vec::<String>::new());
}

/// Two installers: the first, paused after staging, keeps its stage while the second
/// publishes, then finds the widget current; exactly one render and one history line,
/// naming the second.
#[test]
fn two_installers_never_touch_each_others_stage() {
    let desk = Desk::new();
    let pause = Pause::new(desk.root());
    let first = desk.spawn(&[("OMADDONS_FAILPOINTS", "omarchy.staged=pause"), pause.env()]);
    pause.reached("omarchy.staged");
    let theirs = stages(&desk);
    assert_eq!(theirs.len(), 1);

    let d = desk.install();
    assert_eq!(d["data"]["installed"], "fresh");
    assert_eq!(stages(&desk), theirs, "a live stage is never reclaimed");
    let published = tree(&desk.plugin());

    pause.resume("omarchy.staged");
    let r = wait(first, &["omarchy", "install"]);
    let d = r.expect(0);
    assert_eq!(d["data"]["installed"], "unchanged", "{d}");
    assert_eq!(d["data"]["enabled"], "already");
    assert_eq!(tree(&desk.plugin()), published);
    let lines = desk.widget_lines();
    assert_eq!(lines.len(), 1);
    let first_op = theirs[0].trim_start_matches(".omaddons.stage-");
    assert_ne!(lines[0]["tx"], first_op);
    assert_eq!(desk.leftovers(), Vec::<String>::new());
    assert_eq!(desk.inflight_locks(), 0);
}

/// An install racing a removal (the widget deleted while it waits) publishes as a first
/// install and leaves nothing hidden. E3's `omarchy remove` takes the place of the
/// deletion once it exists.
#[test]
fn an_install_racing_a_removal_publishes_as_a_first_install() {
    let desk = Desk::new();
    let (_, new) = tampered(&desk);
    let pause = Pause::new(desk.root());
    let child = desk.spawn(&[("OMADDONS_FAILPOINTS", "omarchy.staged=pause"), pause.env()]);
    pause.reached("omarchy.staged");
    fs::remove_dir_all(desk.plugin()).unwrap();
    pause.resume("omarchy.staged");
    let d = wait(child, &["omarchy", "install"]).expect(0);
    assert_eq!(d["data"]["installed"], "fresh", "{d}");
    assert_eq!(tree(&desk.plugin()), new);
    assert_eq!(desk.leftovers(), Vec::<String>::new());
}

/// A stage whose installer was killed is reclaimed by the next install.
#[test]
fn an_abandoned_stage_is_reclaimed() {
    let desk = Desk::new();
    let pause = Pause::new(desk.root());
    let mut child = desk.spawn(&[("OMADDONS_FAILPOINTS", "omarchy.staged=pause"), pause.env()]);
    pause.reached("omarchy.staged");
    child.kill().unwrap();
    child.wait().unwrap();
    assert_eq!(stages(&desk).len(), 1);
    assert_eq!(desk.inflight_locks(), 1);

    let d = desk.install();
    assert_eq!(d["data"]["installed"], "fresh");
    assert_eq!(desk.leftovers(), Vec::<String>::new());
    assert_eq!(desk.inflight_locks(), 0);
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
        text.contains("enabled it in the bar's right section"),
        "{text}"
    );
    assert!(text.contains("omaddons omarchy install"), "{text}");
}
