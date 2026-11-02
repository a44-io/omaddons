//! A stub Omarchy desk (lifted from `tests/omarchy.rs`): `<config>/omarchy` and stub
//! `omarchy-*` tools that log their argv and `OMARCHY_PATH` to `<root>/tools.log`.
//! omaddons runs with `PATH` holding the stubs alone, so the real tools (whose
//! `omarchy-bar put` would place the widget on this machine's bar) are never reachable;
//! the stubs use shell builtins only.

use std::fs;
use std::path::{Path, PathBuf};

use super::env::TestEnv;
use super::exec::write_executable;
use super::home::{Home, Run};

/// The bar widget's plugin id.
pub const WIDGET_ID: &str = "a44.omaddons";

/// Every stub tool a desk puts on `PATH`.
pub const TOOLS: [&str; 4] = [
    "omarchy-shell",
    "omarchy-plugin-validate",
    "omarchy-bar",
    "omarchy-plugin-disable",
];

/// Writes the stub tool `name` into `<root>/bin/`. `omarchy-bar` answers as
/// `<root>/bar-mode` says (`ok`, `not-running`, `unknown:<n>` for n "not a known widget"
/// answers first) and, placing, writes the section into `shell.json` as Omarchy's shell
/// would; `omarchy-plugin-validate` refuses when `<root>/validate-mode` says `bad`.
/// `omarchy-shell` answers `shell ping` as `<root>/shell-mode` says (`ok`, `silent` for
/// `-q`'s bare exit 0, `hang`, `orphan` for a hang that leaves a child running, its pid in
/// `<root>/orphan.pid`), and `setPluginEnabled`, like `omarchy-plugin-disable` (which
/// fails when `<root>/disable-mode` says `fail`), splices the bar layout out of
/// `shell.json`.
pub fn write_tool(at: &Path, name: &str) {
    let root = at.display();
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
        "omarchy-shell" => format!(
            r#"mode=ok; [ -f {root}/shell-mode ] && read mode < {root}/shell-mode
case "$1 $2" in
  "shell ping") case "$mode" in
      silent) exit 0 ;;
      hang) exec /bin/sleep 5 ;;
      orphan) /bin/sleep 30 & echo $! > {root}/orphan.pid; exec /bin/sleep 5 ;;
      daemon) /bin/sleep 30 & echo $! > {root}/orphan.pid ;;
    esac; echo ok ;;
  "shell setPluginEnabled")
    printf '{{"bar":{{"layout":{{}}}}}}' > "$XDG_CONFIG_HOME/omarchy/shell.json"; echo ok ;;
esac"#
        ),
        "omarchy-plugin-disable" => format!(
            r#"mode=ok; [ -f {root}/disable-mode ] && read mode < {root}/disable-mode
if [ "$mode" = fail ]; then echo "omarchy-plugin-disable: plugin '$1' is not known" >&2; exit 1; fi
printf '{{"bar":{{"layout":{{}}}}}}' > "$XDG_CONFIG_HOME/omarchy/shell.json"
echo "Disabled $1""#
        ),
        _ => String::new(),
    };
    let path = at.join("bin").join(name);
    write_executable(&path, &format!("#!/bin/sh\n{log}\n{body}\n"));
}

/// A stub desk in a test environment: `<config>/omarchy`, every stub tool in
/// `<root>/bin/`, and `<root>/omarchy/shell/`, so a run given only [`StubDesk::omarchy_path`]
/// as `OMARCHY_PATH` (a doctor or `status` run, which starts no Omarchy tool) finds
/// Omarchy's shell installed too.
pub struct StubDesk {
    root: PathBuf,
    config: PathBuf,
}

/// The stub desk of `env`, written on first use (writing it again changes nothing).
pub fn stub_desk(env: &TestEnv) -> StubDesk {
    let desk = StubDesk {
        root: env.root().to_path_buf(),
        config: env.config(),
    };
    fs::create_dir_all(desk.config.join("omarchy")).unwrap();
    fs::create_dir_all(desk.root.join("bin")).unwrap();
    fs::create_dir_all(desk.omarchy_path().join("shell")).unwrap();
    for tool in TOOLS {
        write_tool(&desk.root, tool);
    }
    desk
}

impl StubDesk {
    /// `<root>/bin`, the stubs: the whole `PATH` of a run that may start Omarchy's tools.
    pub fn bin(&self) -> PathBuf {
        self.root.join("bin")
    }

    /// `<root>/omarchy`, the run's `OMARCHY_PATH` (`common::doctor::plant::omarchy_path`).
    pub fn omarchy_path(&self) -> PathBuf {
        self.root.join("omarchy")
    }

    /// `<config>/omarchy/plugins/omaddons`, where `omarchy install` puts the widget.
    pub fn plugin(&self) -> PathBuf {
        self.config.join("omarchy/plugins/omaddons")
    }

    /// The environment of a run that may start Omarchy's tools: `PATH` holding the stubs
    /// alone, and `OMARCHY_PATH`.
    pub fn env(&self) -> [(&'static str, PathBuf); 2] {
        [("PATH", self.bin()), ("OMARCHY_PATH", self.omarchy_path())]
    }

    /// `omaddons omarchy install --json` on `home` with the stubs: this binary's widget,
    /// rendered for `home`'s directories. Asserts exit 0.
    pub fn install(&self, home: &Home) -> Run {
        let env = self.env();
        let extra: Vec<(&str, &str)> = env
            .iter()
            .map(|(key, value)| (*key, value.to_str().unwrap()))
            .collect();
        let args = ["omarchy", "install", "--json"].map(str::to_owned);
        let run = home.exec_with(&args, &extra);
        assert_eq!(run.code, 0, "omarchy install\n{}", run.context());
        run
    }
}
