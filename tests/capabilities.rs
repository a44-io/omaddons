//! e2e suite: capabilities
//!
//! `omaddons capabilities` end to end, through the real binary (A4, M4-D4): the round
//! trip of every command, flag, value, alias, conflict, requirement and pointer it lists,
//! against the binary's own parser and help; its completeness against every registry;
//! purity (no request, no write, hashed trees, nothing created on a fresh home); broken
//! configurations and an unusable `HOME`, which still answer; effective values with their
//! source; credentials detected without being read or run; the human summary; and the
//! global flags that change nothing. The checks live in `tests/common/capabilities.rs`,
//! which T2's slice-1 demo reuses; the planted-fault tests at the end prove each check
//! catches what it names.
//!
//! The data is about 300 KB, so the transcripts log its digest, and large comparisons are
//! made as the list of what differs, expected empty.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use common::capabilities::{completeness, digest, fetch, round_trip, sample_argv};
use common::env::TestEnv;
use common::home::Home;
use common::procfs::running_as_root;
use common::tlog::{TestLog, tlog};

/// A planted GitHub token: fake, and registered with each log as a secret.
// ubs:ignore — a canary to find leaks, not a credential
const TOKEN: &str = "ghp_A4plantedFakeToken0123456789abcdefgh";

/// `capabilities`' data with no config and no credential, built in-process.
fn builtin_data() -> Value {
    let registry = omaddons::contract::Registry::builtin();
    let data = omaddons::app::capabilities::data_of(&registry).unwrap();
    serde_json::to_value(&data).unwrap()
}

/// `<config>/omaddons/config.toml` of `env`.
fn config_file(env: &TestEnv) -> PathBuf {
    env.config().join("omaddons").join("config.toml")
}

/// The stored GitHub token file of `env`.
fn token_file(env: &TestEnv) -> PathBuf {
    env.config()
        .join("omaddons")
        .join("credentials")
        .join("github")
}

/// Removes whatever stands at `path`: a file, a directory, or nothing.
fn clear(path: &Path) {
    if let Ok(meta) = fs::symlink_metadata(path) {
        if meta.is_dir() {
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
            fs::remove_dir_all(path).unwrap();
        } else {
            fs::remove_file(path).unwrap();
        }
    }
}

/// `env`'s config.toml holding exactly `bytes`.
fn write_config(env: &TestEnv, bytes: &[u8]) {
    let path = config_file(env);
    clear(&path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, bytes).unwrap();
}

/// `env`'s stored GitHub token, mode 0600.
fn store_token(env: &TestEnv) {
    let path = token_file(env);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, TOKEN).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
}

/// `capabilities --json` on `env` with extra variables; its envelope's `data`.
#[track_caller]
fn data_with(log: &TestLog, env: &TestEnv, extra: &[(&str, &str)]) -> (Value, Vec<String>) {
    let mut cmd = log.cmd(env, &["capabilities", "--json"]);
    for (key, value) in extra {
        cmd = cmd.env(key, value);
    }
    let ran = cmd.run();
    ran.ok();
    log.json("digest", &digest(ran.stdout()));
    assert!(
        !ran.stdout().contains(TOKEN) && !ran.stderr().contains(TOKEN),
        "the token is never printed"
    );
    (ran.data().clone(), ran.warnings())
}

/// The entry of `data[list]` whose `key` is `name`.
fn entry<'a>(data: &'a Value, list: &str, key: &str, name: &str) -> &'a Value {
    data[list]
        .as_array()
        .and_then(|entries| entries.iter().find(|entry| entry[key] == name))
        .unwrap_or_else(|| panic!("{list}: no {key} {name}")) // ubs:ignore — test lookup
}

/// What differs between `got` and the built-in `want` for the values config can change,
/// entry by entry, leaving out the entries `overridden` names (`list/name`).
fn differences_from_builtin(got: &Value, want: &Value, overridden: &[&str]) -> Vec<String> {
    let mut out = Vec::new();
    for (list, key) in [
        ("limits", "name"),
        ("sources", "id"),
        ("families", "name"),
        ("client_rules", "row"),
    ] {
        for expected in want[list].as_array().into_iter().flatten() {
            let name = expected[key].as_str().unwrap_or_default();
            if overridden.contains(&format!("{list}/{name}").as_str()) {
                continue;
            }
            let actual = got[list]
                .as_array()
                .and_then(|entries| entries.iter().find(|entry| entry[key] == name));
            // ubs:ignore — comparing published values, not secrets
            if actual != Some(expected) {
                out.push(format!("{list}/{name}: {actual:?}, expected {expected}"));
            }
        }
    }
    out
}

/// The config_error a broken file at `path` holding `text` gives, from the library's own
/// parser.
fn parse_problem(path: &Path, text: &str) -> Value {
    let problem = omaddons::state::config::Config::parse_checked(text, path).unwrap_err();
    json!({
        "path": path.display().to_string(),
        "line": problem.line,
        "column": problem.column,
        "message": problem.message,
    })
}

/// The config_error a file `path` that cannot be read gives: its I/O error.
fn read_problem(path: &Path) -> Value {
    let error = fs::read_to_string(path).unwrap_err();
    json!({
        "path": path.display().to_string(),
        "line": null,
        "column": null,
        "message": error.to_string(),
    })
}

#[test]
fn capabilities_round_trips() {
    let log = tlog!();
    let home = Home::new(&["wow", "wow_classic"]);
    log.step("fetch");
    let data = fetch(&log, &home);

    log.step("the round trip");
    let failures = round_trip(&data);
    log.json("failures", &failures);
    assert!(
        failures.is_empty(),
        "{} round-trip failures:\n{}",
        failures.len(),
        failures.join("\n")
    );

    log.step("help through the binary");
    let commands = data["commands"].as_array().unwrap();
    for command in commands {
        let mut argv: Vec<String> = command["path"]
            .as_str()
            .unwrap()
            .split(' ')
            .map(str::to_owned)
            .collect();
        argv.extend(["--help".to_owned(), "--json".to_owned()]);
        let ran = log.run(&home, &argv);
        ran.ok();
        assert_eq!(ran.envelope()["command"], "help", "{argv:?}");
    }

    log.step("pointers run");
    let mut pointers: Vec<&str> = [&data["schemas_uri"], &data["robot_docs_uri"]]
        .into_iter()
        .filter_map(Value::as_str)
        .collect();
    pointers.extend(commands.iter().filter_map(|c| c["data_schema"].as_str()));
    log.json("pointers", &pointers.len());
    for pointer in pointers {
        let mut argv = shell_words::split(pointer).unwrap();
        argv.remove(0);
        // ubs:ignore — comparing argv words, not secrets
        if !argv.iter().any(|word| word == "--json") {
            argv.push("--json".to_owned());
        }
        log.run(&home, &argv).ok();
    }
}

#[test]
fn capabilities_lists_every_registered_name_exactly_once() {
    let log = tlog!();
    let home = Home::new(&["wow", "wow_classic"]);
    log.step("fetch");
    let data = fetch(&log, &home);

    log.step("completeness");
    let failures = completeness(&data);
    log.json("failures", &failures);
    assert!(
        failures.is_empty(),
        "{} completeness failures:\n{}",
        failures.len(),
        failures.join("\n")
    );
    let len = |key: &str| data[key].as_array().map_or(0, Vec::len);
    log.assert_eq("commands", &len("commands"), &omaddons::app::COMMANDS.len());
    log.assert_eq(
        "error codes",
        &len("error_codes"),
        &omaddons::error::ErrorCode::ALL.len(),
    );
    log.assert_eq(
        "warning codes",
        &len("warning_codes"),
        &omaddons::error::WarningCode::ALL.len(),
    );
    log.assert_eq(
        "actions",
        &len("actions"),
        &omaddons::contract::actions::Action::ALL.len(),
    );
}

#[test]
fn capabilities_reads_only_config_and_writes_nothing() {
    let log = tlog!();
    log.secret(TOKEN);

    // A used home: state, cache, a config and a stored token are there.
    let home = Home::new(&["wow", "wow_classic"]);
    let env = &home.env;
    log.step("prepare");
    log.run(&home, &["status", "--json"]).ok();
    write_config(
        env,
        b"git_hosts = [\"codeberg.org\"]\n[limits]\ndownload_max_bytes = \"1GiB\"\n",
    );
    store_token(env);

    log.step("hash");
    let trees: Vec<(String, PathBuf)> = vec![
        ("state".to_owned(), env.state()),
        ("cache".to_owned(), env.cache()),
        ("config".to_owned(), env.config()),
        ("retail".to_owned(), home.client("_retail_")),
        ("classic".to_owned(), home.client("_classic_")),
    ];
    for (label, root) in &trees {
        assert!(log.tree(label, root).is_some(), "{label} exists");
    }

    log.step("run");
    let json_run = log.run(&home, &["capabilities", "--json"]);
    json_run.ok();
    log.json("digest", &digest(json_run.stdout()));
    let github = entry(json_run.data(), "sources", "id", "github");
    assert_eq!(github["credential_configured"], true, "the stored token");
    let human = log.run(&home, &["capabilities"]);
    human.ok();

    log.step("unchanged");
    for (label, root) in &trees {
        log.assert_unchanged(label, root);
    }
    let net_log = env.state().join("omaddons").join("net.log");
    assert!(!net_log.exists(), "no request was ever logged");
    for ran in [&json_run, &human] {
        assert!(
            !ran.stdout().contains(TOKEN) && !ran.stderr().contains(TOKEN),
            "the token is never printed"
        );
    }

    // A fresh home: nothing is created.
    log.step("a fresh home");
    let fresh = TestEnv::new();
    log.run(&fresh, &["capabilities", "--json"]).ok();
    for dir in [fresh.state(), fresh.cache()] {
        assert!(!dir.join("omaddons").exists(), "{} created", dir.display());
    }
}

#[test]
fn a_broken_config_still_answers() {
    let log = tlog!();
    let env = TestEnv::new();
    let builtin = builtin_data();
    let path = config_file(&env);
    let check = |case: &str, data: &Value, expected: &Value| {
        log.assert_eq(
            &format!("{case}: config_error"),
            &data["config_error"],
            expected,
        );
        let differences = differences_from_builtin(data, &builtin, &[]);
        log.json("differences from the built-in values", &differences);
        assert!(differences.is_empty(), "{case}: {differences:#?}");
    };

    for (case, text) in [
        ("malformed_toml", "limits = [\n"),
        ("out_of_range", "check_interval_hours = 99\n"),
        ("bad_git_host", "git_hosts = [\"https://x\"]\n"),
    ] {
        log.step(case);
        write_config(&env, text.as_bytes());
        let (data, _) = data_with(&log, &env, &[]);
        check(case, &data, &parse_problem(&path, text));
    }

    log.step("unreadable");
    if running_as_root() {
        log.note("running as root, which reads a mode-000 file: case skipped");
    } else {
        write_config(&env, b"default_channel = \"beta\"\n");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
        let expected = read_problem(&path);
        let (data, _) = data_with(&log, &env, &[]);
        check("unreadable", &data, &expected);
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    }

    log.step("a_directory");
    clear(&path);
    fs::create_dir_all(&path).unwrap();
    let expected = read_problem(&path);
    let (data, _) = data_with(&log, &env, &[]);
    check("a_directory", &data, &expected);

    log.step("invalid_utf8");
    write_config(&env, b"default_channel = \"\xff\xfe\"\n");
    let expected = read_problem(&path);
    let (data, _) = data_with(&log, &env, &[]);
    check("invalid_utf8", &data, &expected);

    log.step("relative_home");
    clear(&path);
    let message = omaddons::state::paths::Paths::from_lookup(|name| {
        (name == "HOME").then(|| "relative/home".into())
    })
    .unwrap_err()
    .message
    .clone();
    let (data, _) = data_with(&log, &env, &[("HOME", "relative/home")]);
    check(
        "relative_home",
        &data,
        &json!({"path": null, "line": null, "column": null, "message": message}),
    );

    log.step("unknown_key");
    write_config(
        &env,
        b"defualt_target = \"x\"\n[limits]\ndownload_max_bytes = \"1GiB\"\n",
    );
    let (data, warnings) = data_with(&log, &env, &[]);
    log.assert_eq("config_error", &data["config_error"], &Value::Null);
    assert!(
        warnings.iter().any(|code| code == "config_unknown_key"),
        "{warnings:?}"
    );
    let download = entry(&data, "limits", "name", "DOWNLOAD_MAX_BYTES");
    log.assert_eq(
        "DOWNLOAD_MAX_BYTES",
        download,
        &json!({"name": "DOWNLOAD_MAX_BYTES", "value": 1u64 << 30, "unit": "bytes",
                "config_key": "limits.download_max_bytes", "source": "config"}),
    );
}

#[test]
fn effective_values_carry_their_source() {
    let log = tlog!();
    let env = TestEnv::new();
    let builtin = builtin_data();

    log.step("overrides");
    write_config(
        &env,
        br#"check_interval_hours = 6
git_hosts = ["codeberg.org"]

[limits]
download_max_bytes = "1GiB"
tx_retention_count = 5
tx_retention_days = 3
backup_min_free_mb = 1

[sources.wowi]
enabled = false

[sources.curseforge]
enabled = true

[sources.wago]
enabled = true

[rules.family_ranges]
vanilla = { min = 11500, max = 11599 }

[client_rules.tbc]
game_types = ["tbc", "bcc"]

[client_rules.custom-test]
family = "wrath-legacy"
version_min = "3.3.6"
multi_toc = false
comma_interface = false
interface_includes_patch = false
legacy = true
"#,
    );
    let (data, warnings) = data_with(&log, &env, &[]);
    log.assert_eq("warnings", &warnings, &Vec::<String>::new());
    log.assert_eq("config_error", &data["config_error"], &Value::Null);
    let source =
        |list: &str, key: &str, name: &str| entry(&data, list, key, name)["source"].clone();
    let config = json!("config");

    let day_ms = 24 * 60 * 60 * 1000_u64;
    for (name, value) in [
        ("DOWNLOAD_MAX_BYTES", json!(1u64 << 30)),
        ("DEFAULT_CHECK_INTERVAL", json!(6 * 60 * 60 * 1000_u64)),
        ("BACKUP_MIN_FREE", json!(1u64 << 20)),
    ] {
        let limit = entry(&data, "limits", "name", name);
        log.assert_eq(
            name,
            &(&limit["value"], &limit["source"]),
            &(&value, &config),
        );
    }
    let retention = entry(&data, "limits", "name", "TX_RETENTION");
    log.assert_eq(
        "TX_RETENTION",
        &(
            &retention["value"]["keep_last"],
            &retention["value"]["window_ms"],
            &retention["source"],
        ),
        &(&json!(5), &json!(3 * day_ms), &config),
    );
    let wowi = entry(&data, "sources", "id", "wowi");
    log.assert_eq(
        "wowi",
        &(&wowi["enabled"], &wowi["source"]),
        &(&json!(false), &config),
    );
    let git = entry(&data, "sources", "id", "git");
    log.assert_eq(
        "git",
        &(
            git["hosts"].as_array().and_then(|h| h.last()),
            &git["source"],
        ),
        &(Some(&json!("codeberg.org")), &config),
    );
    // Neither is in this build, whatever config.toml says.
    for id in ["curseforge", "wago"] {
        let later = entry(&data, "sources", "id", id);
        log.assert_eq(
            id,
            &(&later["enabled"], &later["source"]),
            &(&json!(false), &json!("builtin")),
        );
    }
    let vanilla = entry(&data, "families", "name", "vanilla");
    log.assert_eq(
        "vanilla",
        &(
            &vanilla["interface_min"],
            &vanilla["interface_max"],
            &vanilla["source"],
        ),
        &(&json!(11500), &json!(11599), &config),
    );
    let tbc = entry(&data, "client_rules", "row", "tbc");
    log.assert_eq(
        "tbc",
        &(&tbc["game_types"], &tbc["source"]),
        &(&json!(["tbc", "bcc"]), &config),
    );
    log.assert_eq(
        "the new row comes first",
        &(
            &data["client_rules"][0]["row"],
            &source("client_rules", "row", "custom-test"),
        ),
        &(&json!("custom-test"), &config),
    );
    let differences = differences_from_builtin(
        &data,
        &builtin,
        &[
            "limits/DOWNLOAD_MAX_BYTES",
            "limits/DEFAULT_CHECK_INTERVAL",
            "limits/TX_RETENTION",
            "limits/BACKUP_MIN_FREE",
            "sources/wowi",
            "sources/git",
            "families/vanilla",
            "client_rules/tbc",
        ],
    );
    log.json("differences from the built-in values", &differences);
    assert!(differences.is_empty(), "{differences:#?}");

    log.step("zero hours");
    write_config(&env, b"check_interval_hours = 0\n");
    let (data, _) = data_with(&log, &env, &[]);
    let interval = entry(&data, "limits", "name", "DEFAULT_CHECK_INTERVAL");
    log.assert_eq(
        "DEFAULT_CHECK_INTERVAL",
        interval,
        entry(&builtin, "limits", "name", "DEFAULT_CHECK_INTERVAL"),
    );
}

#[test]
fn credentials_are_detected_but_never_read_or_run() {
    let log = tlog!();
    log.secret(TOKEN);
    let env = TestEnv::new();
    let github =
        |data: &Value| entry(data, "sources", "id", "github")["credential_configured"].clone();

    log.step("none");
    let (data, _) = data_with(&log, &env, &[]);
    log.assert_eq("none", &github(&data), &json!(false));

    log.step("token_variable");
    let (data, _) = data_with(&log, &env, &[("OMADDONS_GITHUB_TOKEN", TOKEN)]);
    log.assert_eq("token_variable", &github(&data), &json!(true));

    log.step("stored_file_unreadable");
    if running_as_root() {
        log.note("running as root, which reads a mode-000 file: case skipped");
    } else {
        store_token(&env);
        fs::set_permissions(token_file(&env), fs::Permissions::from_mode(0o000)).unwrap();
        let (data, warnings) = data_with(&log, &env, &[]);
        log.assert_eq("stored_file_unreadable", &github(&data), &json!(true));
        assert!(
            // ubs:ignore — warning codes are public enum names, not secrets
            !warnings.iter().any(|code| code == "credential_file_mode"),
            "{warnings:?}"
        );
        fs::set_permissions(token_file(&env), fs::Permissions::from_mode(0o600)).unwrap();
    }
    clear(&token_file(&env));

    log.step("command_reference");
    let marker = env.root().join("credential-command-ran");
    write_config(
        &env,
        format!(
            "[sources.github]\ncredential = {{ command = [\"sh\", \"-c\", \"touch {}\"] }}\n",
            marker.display()
        )
        .as_bytes(),
    );
    let (data, _) = data_with(&log, &env, &[]);
    log.assert_eq("command_reference", &github(&data), &json!(true));
    assert!(!marker.exists(), "the credential command never ran");

    log.step("env_reference_unset");
    write_config(
        &env,
        b"[sources.github]\ncredential = { env = \"OMADDONS_A4_UNSET\" }\n",
    );
    let (data, _) = data_with(&log, &env, &[]);
    log.assert_eq("env_reference_unset", &github(&data), &json!(false));

    log.step("file_reference_missing");
    let missing = env.root().join("no-such-token-file");
    write_config(
        &env,
        format!(
            "[sources.github]\ncredential = {{ file = \"{}\" }}\n",
            missing.display()
        )
        .as_bytes(),
    );
    let (data, _) = data_with(&log, &env, &[]);
    log.assert_eq("file_reference_missing", &github(&data), &json!(false));
}

#[test]
fn the_human_form_is_a_summary() {
    let log = tlog!();
    let env = TestEnv::new();

    log.step("the summary");
    let ran = log.run(&env, &["capabilities"]);
    ran.ok();
    let text = ran.stdout();
    let lines: Vec<&str> = text.lines().collect();
    log.json("lines", &lines.len());
    assert!(lines.len() <= 40, "{} lines\n{text}", lines.len());
    for line in &lines {
        assert!(line.chars().count() < 80, "{line:?}\n{text}");
    }
    let first = format!(
        "omaddons {} · contract {}",
        env!("CARGO_PKG_VERSION"),
        omaddons::output::SCHEMA_VERSION
    );
    assert!(
        lines.first().is_some_and(|l| l.starts_with(&first)),
        "{text}"
    );
    assert_eq!(
        lines.last(),
        Some(&"--json for the full description"),
        "{text}"
    );
    for exit in omaddons::error::Exit::ALL {
        assert!(text.contains(exit.name()), "{}\n{text}", exit.name());
    }
    for id in omaddons::sources::SourceId::ALL {
        assert!(text.contains(id.wire()), "{}\n{text}", id.wire());
    }
    assert!(!text.contains('\u{1b}'), "no escape sequence\n{text}");
    log.assert_eq("stderr", &ran.stderr(), &"");

    log.step("with a broken config");
    write_config(&env, b"limits = [\n");
    let ran = log.run(&env, &["capabilities"]);
    ran.ok();
    assert!(
        ran.stdout()
            .lines()
            .any(|line| line.starts_with("config.toml line")),
        "{}",
        ran.stdout()
    );
    assert!(!ran.stdout().contains(&env.config().display().to_string()));
}

#[test]
fn flags_that_do_not_apply_change_nothing() {
    let log = tlog!();
    let env = TestEnv::new();
    log.step("plain");
    let (plain, _) = data_with(&log, &env, &[]);
    for extra in [
        &["-t", "nosuch"][..],
        &["--offline"],
        &["--dry-run"],
        &["--actor", "agent", "--reason", "x"],
    ] {
        log.step(&extra.join(" "));
        let mut argv = vec!["capabilities", "--json"];
        argv.extend(extra);
        let ran = log.run(&env, &argv);
        ran.ok();
        // ubs:ignore — comparing published data, not secrets
        let same = ran.data() == &plain;
        log.assert_eq("data as the plain run's", &same, &true);
    }
    log.step("--if-match");
    let token = format!("pc1:{}", "0".repeat(64));
    log.run(&env, &["capabilities", "--json", "--if-match", &token])
        .fails(2, "usage");
}

// Each check catches what it names: one planted fault, one failure.

/// `planted` applied to the built-in data, then `check`: exactly one failure, holding
/// every one of `says`.
fn one_failure(
    case: &str,
    check: fn(&Value) -> Vec<String>,
    planted: impl FnOnce(&mut Value),
    says: &[&str],
) {
    let mut data = builtin_data();
    planted(&mut data);
    let failures = check(&data);
    assert_eq!(failures.len(), 1, "{case}: {failures:#?}");
    for word in says {
        assert!(
            failures[0].contains(word),
            "{case}: {word:?} in {}",
            failures[0]
        );
    }
}

/// The `commands[]` entry of `path` in `data`.
fn command_mut<'a>(data: &'a mut Value, path: &str) -> &'a mut Value {
    data["commands"]
        .as_array_mut()
        // ubs:ignore — comparing command paths, not secrets
        .and_then(|commands| commands.iter_mut().find(|c| c["path"] == path))
        .unwrap()
}

/// The `flags[]` entry `long` of `command`.
fn flag_mut<'a>(command: &'a mut Value, long: &str) -> &'a mut Value {
    command["flags"]
        .as_array_mut()
        .and_then(|flags| flags.iter_mut().find(|f| f["long"] == long))
        .unwrap()
}

fn push(value: &mut Value, item: &str) {
    value.as_array_mut().unwrap().push(json!(item));
}

#[test]
fn round_trip_reports_each_planted_fault() {
    assert_eq!(round_trip(&builtin_data()), Vec::<String>::new());
    one_failure(
        "a flag clap does not know",
        round_trip,
        |data| flag_mut(command_mut(data, "install"), "channel")["long"] = json!("channnel"),
        &["install", "--channnel"],
    );
    one_failure(
        "a value clap refuses",
        round_trip,
        |data| {
            push(
                &mut flag_mut(command_mut(data, "install"), "channel")["possible_values"],
                "gamma",
            )
        },
        &["install", "--channel gamma"],
    );
    one_failure(
        "a conflict that is not one",
        round_trip,
        |data| {
            push(
                &mut flag_mut(command_mut(data, "install"), "channel")["conflicts_with"],
                "dry-run",
            )
        },
        &["install", "--channel with dry-run", "expected a conflict"],
    );
    one_failure(
        "a requirement that is not one",
        round_trip,
        |data| {
            push(
                &mut flag_mut(command_mut(data, "install"), "channel")["requires"],
                "json",
            )
        },
        &[
            "install",
            "--channel without --json",
            "expected a missing requirement",
        ],
    );
    one_failure(
        "another command's alias",
        round_trip,
        |data| push(&mut command_mut(data, "install")["aliases"], "status"),
        &["install", "alias status"],
    );
    one_failure(
        "a pointer that does not parse",
        round_trip,
        |data| data["schemas_uri"] = json!("omaddons robot-docs schemaa"),
        &["schemas_uri", "does not parse"],
    );
}

#[test]
fn completeness_reports_each_planted_fault() {
    assert_eq!(completeness(&builtin_data()), Vec::<String>::new());
    one_failure(
        "an error code removed",
        completeness,
        |data| {
            data["error_codes"].as_array_mut().unwrap().remove(0);
        },
        &["error_codes", "missing", "usage"],
    );
    one_failure(
        "an action listed twice",
        completeness,
        |data| {
            let actions = data["actions"].as_array_mut().unwrap();
            let first = actions[0].clone();
            actions.push(first);
        },
        &["actions", "listed twice"],
    );
    one_failure(
        "commands reordered",
        completeness,
        |data| data["commands"].as_array_mut().unwrap().swap(0, 1),
        &["commands", "order differs"],
    );
    one_failure(
        "a wrong exit_codes",
        completeness,
        |data| command_mut(data, "install")["exit_codes"] = json!([0]),
        &["commands install: exit_codes"],
    );
    one_failure(
        "a binding command without --if-match",
        completeness,
        |data| {
            let install = command_mut(data, "install");
            let flags = install["flags"].as_array_mut().unwrap();
            flags.retain(|flag| flag["long"] != "if-match");
        },
        &["commands install: globals", "if-match"],
    );
    one_failure(
        "a wrong count",
        completeness,
        |data| {
            let count = data["failure_modes"]["count"].as_u64().unwrap();
            data["failure_modes"]["count"] = json!(count + 1);
        },
        &["failure_modes.count"],
    );
}

#[test]
fn sample_argv_supplies_required_positionals() {
    let data = builtin_data();
    let command = |path: &str| {
        data["commands"]
            .as_array()
            // ubs:ignore — comparing command paths, not secrets
            .and_then(|c| c.iter().find(|c| c["path"] == path))
            .cloned()
            .unwrap()
    };
    for (path, want) in [
        ("install", &["omaddons", "install", "1"][..]),
        ("targets add", &["omaddons", "targets", "add", "1"]),
        ("help", &["omaddons", "help"]),
        ("doctor gc", &["omaddons", "doctor", "gc", "--before", "1"]),
        ("completions", &["omaddons", "completions", "bash"]),
    ] {
        assert_eq!(sample_argv(&command(path)), want, "{path}");
    }
}
