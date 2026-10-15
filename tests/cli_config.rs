//! `omaddons config` end to end, and config.toml's effect on other commands.

mod common;

use std::fs;

use common::env::TestEnv;
use common::trees::{BattlenetRoot, CustomRoot, Prefix};
use omaddons::fsx::GlobalLock;
use omaddons::state::paths::Paths;
use serde_json::{Value, json};
use support::{run, write_config};

#[cfg(test)]
mod support {
    use super::*;

    /// Runs `omaddons <args> --json`: (exit code, envelope).
    pub fn run(env: &TestEnv, args: &[&str]) -> (i32, Value) {
        let output = env.command().args(args).arg("--json").output().unwrap();
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert_eq!(
            stdout.lines().count(),
            1,
            "one envelope for {args:?}\n{stdout}"
        );
        (
            output.status.code().unwrap(),
            serde_json::from_str(&stdout).unwrap(),
        )
    }

    /// Writes `<config>/omaddons/config.toml`.
    pub fn write_config(env: &TestEnv, text: &str) {
        let path = env.config().join("omaddons/config.toml");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }
}

const COMMENTED: &str = "# Main settings.\ndefault_channel = \"stable\"   # keep stable\n\n[limits]\n# Big snapshots.\nsnapshot_max_bytes = \"2GiB\"\narchive_max_ratio = 150 # trailing\n";

fn config_file(env: &TestEnv) -> std::path::PathBuf {
    env.config().join("omaddons/config.toml")
}

fn setting<'a>(envelope: &'a Value, key: &str) -> &'a Value {
    envelope["data"]["settings"]
        .as_array()
        .and_then(|s| s.iter().find(|s| s["key"] == key))
        .unwrap_or(&Value::Null)
}

#[test]
fn a_missing_file_is_all_defaults() {
    let env = TestEnv::new();
    let (code, envelope) = run(&env, &["config"]);
    assert_eq!(
        (code, &envelope["command"], &envelope["data"]["exists"]),
        (0, &json!("config"), &json!(false))
    );
    assert_eq!(
        setting(&envelope, "default_channel"),
        &json!({"key": "default_channel", "value": "stable", "source": "default"})
    );
    assert_eq!(
        setting(&envelope, "limits.snapshot_max_bytes")["value"],
        2u64 << 30
    );
    let (code, envelope) = run(&env, &["config", "path"]);
    assert_eq!(
        (code, &envelope["command"], &envelope["data"]["exists"]),
        (0, &json!("config path"), &json!(false))
    );
    assert!(
        envelope["data"]["path"]
            .as_str()
            .unwrap()
            .ends_with("omaddons/config.toml")
    );
}

#[test]
fn set_changes_only_its_value() {
    let env = TestEnv::new();
    write_config(&env, COMMENTED);
    let (code, envelope) = run(
        &env,
        &["config", "set", "limits.snapshot_max_bytes", "3GiB"],
    );
    assert_eq!(
        (code, &envelope["command"]),
        (0, &json!("config set")),
        "{envelope}"
    );
    assert_eq!(
        (
            &envelope["data"]["previous"],
            &envelope["data"]["value"],
            &envelope["data"]["changed"]
        ),
        (&json!("2GiB"), &json!("3GiB"), &json!(true))
    );
    assert_eq!(
        fs::read_to_string(config_file(&env)).unwrap(),
        COMMENTED.replace("\"2GiB\"", "\"3GiB\"")
    );
    let (_, got) = run(&env, &["config", "get", "limits.snapshot_max_bytes"]);
    assert_eq!(
        (
            &got["command"],
            &got["data"]["value"],
            &got["data"]["source"]
        ),
        (&json!("config get"), &json!("3GiB"), &json!("file"))
    );
    assert_eq!(got["data"]["default"], 2u64 << 30);

    let (code, envelope) = run(&env, &["config", "set", "sources.github.enabled", "false"]);
    assert_eq!(code, 0, "{envelope}");
    let (_, listed) = run(&env, &["config"]);
    assert_eq!(setting(&listed, "sources.github.enabled")["value"], false);
}

#[test]
fn bad_keys_and_values_change_nothing() {
    let env = TestEnv::new();
    write_config(&env, COMMENTED);
    let (code, envelope) = run(&env, &["config", "set", "limits.snapshot_max_byte", "3GiB"]);
    assert_eq!((code, &envelope["error"]["code"]), (2, &json!("usage")));
    assert_eq!(
        envelope["error"]["fix_argv"],
        json!([
            "omaddons",
            "config",
            "set",
            "limits.snapshot_max_bytes",
            "3GiB"
        ])
    );
    let (code, envelope) = run(&env, &["config", "set", "default_channel", "nightly"]);
    assert_eq!((code, &envelope["error"]["code"]), (2, &json!("usage")));
    assert!(
        envelope["error"]["message"]
            .as_str()
            .unwrap()
            .contains("stable, beta or alpha"),
        "{envelope}"
    );
    assert!(!envelope["error"]["fix"].as_str().unwrap().is_empty());
    let (code, _) = run(&env, &["config", "get", "nope"]);
    assert_eq!(code, 2);
    assert_eq!(fs::read_to_string(config_file(&env)).unwrap(), COMMENTED);
}

#[test]
fn a_broken_file_fails_commands_but_not_path_or_help() {
    let env = TestEnv::new();
    write_config(&env, "default_channel = \"stable\"\n[limits\n");
    let (code, envelope) = run(&env, &["list"]);
    assert_eq!(
        (code, &envelope["error"]["code"]),
        (1, &json!("state_corrupt"))
    );
    assert_eq!(
        (
            &envelope["error"]["details"]["line"],
            &envelope["error"]["details"]["column"]
        ),
        (&json!(2), &json!(8))
    );
    assert!(
        envelope["error"]["message"]
            .as_str()
            .unwrap()
            .contains("line 2")
    );
    assert_eq!(run(&env, &["config", "path"]).0, 0);
    assert_eq!(run(&env, &["help"]).0, 0);
    assert_eq!(run(&env, &[]).0, 0);
}

#[test]
fn unknown_keys_warn() {
    let env = TestEnv::new();
    write_config(&env, "defualt_channel = \"beta\"\n");
    let (code, envelope) = run(&env, &["config"]);
    assert_eq!(code, 0);
    let warning = &envelope["warnings"][0];
    assert_eq!(warning["code"], "config_unknown_key");
    assert!(
        warning["message"]
            .as_str()
            .unwrap()
            .contains("did you mean default_channel?")
    );
    let (code, listed) = run(&env, &["list"]);
    assert_eq!(
        (code, &listed["warnings"][0]["code"]),
        (0, &json!("config_unknown_key"))
    );
}

#[test]
fn dry_runs_write_nothing() {
    let env = TestEnv::new();
    let (code, envelope) = run(
        &env,
        &["config", "set", "default_channel", "beta", "--dry-run"],
    );
    assert_eq!(
        (
            code,
            &envelope["data"]["dry_run"],
            &envelope["data"]["changed"]
        ),
        (0, &json!(true), &json!(true))
    );
    assert!(!config_file(&env).exists());
}

#[test]
fn config_feeds_discovery() {
    let env = TestEnv::new();
    let prefix = Prefix::new(&env, "battlenet");
    BattlenetRoot::at(prefix.wow_root()).product("wow").build();
    let outside = CustomRoot::at(env.root().join("mnt/wotlk")).build();
    let roots = format!("[\"{}\"]", outside.display());
    assert_eq!(run(&env, &["config", "set", "extra_roots", &roots]).0, 0);
    assert_eq!(
        run(&env, &["config", "set", "targets.retail.label", "Main"]).0,
        0
    );
    let (_, targets) = run(&env, &["targets"]);
    let ids: Vec<&str> = targets["data"]["targets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["retail", "wotlk"]);
    let wotlk = &targets["data"]["targets"][1];
    assert_eq!(wotlk["found_by"], "extra_root");
    assert_eq!(targets["data"]["targets"][0]["label"], "Main");
}

#[test]
fn set_waits_for_the_lock() {
    let env = TestEnv::new();
    let paths = Paths::with_roots(env.home(), env.config(), env.state(), env.cache());
    let _guard = GlobalLock::acquire(&paths, "test").unwrap();
    let (code, envelope) = run(&env, &["config", "set", "default_channel", "beta"]);
    assert_eq!((code, &envelope["error"]["code"]), (5, &json!("locked")));
    assert!(!config_file(&env).exists());
}
