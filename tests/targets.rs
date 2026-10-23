//! `omaddons targets` end to end: listing, add, rm, rename, default, --rescan, the lock
//! and --dry-run, through the binary over a synthetic home.

mod common;

use std::fs;

use common::env::TestEnv;
use common::trees::{BattlenetRoot, CustomRoot, Prefix, tree_hash};
use omaddons::fsx::GlobalLock;
use omaddons::state::paths::Paths;
use serde_json::{Value, json};
use support::run;

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
}

fn paths(env: &TestEnv) -> Paths {
    Paths::with_roots(env.home(), env.config(), env.state(), env.cache())
}

fn ids(envelope: &Value) -> Vec<String> {
    envelope["data"]["targets"]
        .as_array()
        .map(|ts| {
            ts.iter()
                .filter_map(|t| t["id"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

fn target<'a>(envelope: &'a Value, id: &str) -> &'a Value {
    envelope["data"]["targets"]
        .as_array()
        .and_then(|ts| ts.iter().find(|t| t["id"] == id))
        .unwrap_or(&Value::Null)
}

/// A Battle.net prefix with retail and Era, and a custom client outside the scan.
fn machine(env: &TestEnv) -> std::path::PathBuf {
    let prefix = Prefix::new(env, "battlenet");
    BattlenetRoot::at(prefix.wow_root())
        .product("wow")
        .product("wow_classic_era")
        .build();
    CustomRoot::at(env.root().join("elsewhere/pserver")).build()
}

#[test]
fn listing_before_any_mutation() {
    let env = TestEnv::new();
    machine(&env);
    let (code, envelope) = run(&env, &["targets"]);
    assert_eq!((code, &envelope["command"]), (0, &json!("targets")));
    assert_eq!(ids(&envelope), ["era", "retail"]);
    for t in envelope["data"]["targets"].as_array().unwrap() {
        assert_eq!(
            (&t["registered"], &t["key"], &t["found_by"]),
            (&json!(false), &Value::Null, &json!("scan"))
        );
        assert!(t["client_dir"].as_str().unwrap().ends_with('_'));
        assert!(t["details"].is_null());
    }
    assert_eq!(
        (&envelope["data"]["default"], &envelope["data"]["rescanned"]),
        (&Value::Null, &json!(false))
    );
}

#[test]
fn adding_a_custom_root_by_name() {
    let env = TestEnv::new();
    let pserver = machine(&env);
    let path = pserver.display().to_string();
    // The preview names it as the run will.
    let (code, preview) = run(
        &env,
        &["targets", "add", &path, "--name", "pserver", "--dry-run"],
    );
    assert_eq!(code, 0, "{preview}");
    assert_eq!(preview["data"]["targets"][0]["id"], "pserver", "{preview}");
    let (code, envelope) = run(&env, &["targets", "add", &path, "--name", "pserver"]);
    assert_eq!(
        (code, &envelope["command"]),
        (0, &json!("targets add")),
        "{envelope}"
    );
    assert_eq!(envelope["data"]["changed"], true);
    let added = &envelope["data"]["targets"][0];
    assert_eq!(
        (&added["id"], &added["found_by"]),
        (&json!("pserver"), &json!("manual"))
    );
    let key = added["key"].as_str().unwrap().to_owned();
    assert_eq!(key.len(), 26);
    let marker: Value = serde_json::from_str(
        &fs::read_to_string(pserver.join("Interface/.omaddons/target.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(marker["key"], key);

    let (code, again) = run(&env, &["targets", "add", &path, "--name", "pserver"]);
    assert_eq!((code, &again["data"]["changed"]), (0, &json!(false)));
    let (_, listed) = run(&env, &["targets"]);
    assert_eq!(
        target(&listed, "pserver")["found_by"],
        "manual",
        "manual roots feed discovery"
    );

    // Added again under another name: renamed, the preview saying so first, the key kept.
    let (code, preview) = run(
        &env,
        &["targets", "add", &path, "--name", "warmane", "--dry-run"],
    );
    assert_eq!(code, 0, "{preview}");
    assert_eq!(
        (
            &preview["data"]["targets"][0]["id"],
            &preview["data"]["changed"]
        ),
        (&json!("warmane"), &json!(true)),
        "{preview}"
    );
    let (code, renamed) = run(&env, &["targets", "add", &path, "--name", "warmane"]);
    assert_eq!(code, 0, "{renamed}");
    let one = &renamed["data"]["targets"][0];
    assert_eq!(
        (&one["id"], &one["key"], &renamed["data"]["changed"]),
        (&json!("warmane"), &json!(key), &json!(true)),
        "{renamed}"
    );
}

/// A path through `..` is the directory it leads to, and is recorded as it.
#[test]
fn adding_through_dot_dot_records_the_directory_itself() {
    let env = TestEnv::new();
    let pserver = machine(&env);
    let path = pserver.join("Interface/..").display().to_string();
    let (code, envelope) = run(&env, &["targets", "add", &path]);
    assert_eq!(code, 0, "{envelope}");
    let dir = envelope["data"]["targets"][0]["client_dir"]
        .as_str()
        .unwrap();
    let real = fs::canonicalize(&pserver).unwrap();
    assert_eq!(dir, real.display().to_string(), "{envelope}");
}

#[test]
fn adding_a_battlenet_root_registers_every_flavor() {
    let env = TestEnv::new();
    let root = BattlenetRoot::at(env.root().join("bnet"))
        .product("wow")
        .product("wowf")
        .build();
    let path = root.display().to_string();
    let (code, envelope) = run(&env, &["targets", "add", &path, "--name", "main"]);
    assert_eq!((code, &envelope["error"]["code"]), (2, &json!("usage")));
    // So is its preview.
    let (code, envelope) = run(
        &env,
        &["targets", "add", &path, "--name", "main", "--dry-run"],
    );
    assert_eq!((code, &envelope["error"]["code"]), (2, &json!("usage")));
    let (code, envelope) = run(&env, &["targets", "add", &path]);
    assert_eq!(code, 0);
    assert_eq!(ids(&envelope), ["forever", "retail"], "by flavor folder");
    let (code, envelope) = run(
        &env,
        &[
            "targets",
            "add",
            &root
                .join("_forever_/Interface/AddOns")
                .display()
                .to_string(),
        ],
    );
    assert_eq!(
        (code, &envelope["data"]["changed"]),
        (0, &json!(false)),
        "an AddOns path normalizes"
    );

    let empty = env.root().join("empty");
    fs::create_dir_all(&empty).unwrap();
    let (code, envelope) = run(&env, &["targets", "add", &empty.display().to_string()]);
    assert_eq!(
        (code, &envelope["error"]["code"]),
        (3, &json!("target_not_found"))
    );
    assert_eq!(
        envelope["error"]["fix_argv"],
        json!(["omaddons", "targets"])
    );
}

#[test]
fn renaming_moves_the_selector() {
    let env = TestEnv::new();
    machine(&env);
    let (code, envelope) = run(&env, &["targets", "rename", "retail", "main"]);
    assert_eq!(
        (code, &envelope["command"]),
        (0, &json!("targets rename")),
        "{envelope}"
    );
    assert_eq!(
        (&envelope["data"]["from"], &envelope["data"]["to"]),
        (&json!("retail"), &json!("main"))
    );
    assert!(
        envelope["data"]["key"].is_string(),
        "an unregistered target is registered first"
    );
    assert_eq!(run(&env, &["list", "-t", "main"]).0, 0);
    assert_eq!(run(&env, &["list", "-t", "retail"]).0, 3);
    for bad in ["Main", "01JB8Q2V9W0000000000000000", "a@b", "era"] {
        let (code, envelope) = run(&env, &["targets", "rename", "main", bad]);
        assert_eq!(
            (code, &envelope["error"]["code"]),
            (2, &json!("usage")),
            "{bad}"
        );
    }
    let (code, _) = run(&env, &["targets", "rename", "era", "classic-era"]);
    assert_eq!(code, 0);
    let (code, envelope) = run(&env, &["targets", "rename", "main", "classic-era"]);
    assert_eq!(code, 2, "taken");
    assert!(
        envelope["error"]["message"]
            .as_str()
            .unwrap()
            .contains("_classic_era_"),
        "names the holder"
    );
}

#[test]
fn removing_keeps_the_marker_and_state() {
    let env = TestEnv::new();
    let pserver = machine(&env);
    let path = pserver.display().to_string();
    let (_, added) = run(&env, &["targets", "add", &path, "--name", "pserver"]);
    let key = added["data"]["targets"][0]["key"]
        .as_str()
        .unwrap()
        .to_owned();
    let record = env.state().join(format!("omaddons/targets/{key}.json"));
    fs::create_dir_all(record.parent().unwrap()).unwrap();
    fs::write(
        &record,
        format!(
            "{{\"schema_version\":\"1.0\",\"key\":\"{key}\",\"generation\":0,\"addons\":{{}}}}"
        ),
    )
    .unwrap();

    let (code, envelope) = run(&env, &["targets", "rm", "pserver"]);
    assert_eq!(
        (code, &envelope["command"]),
        (0, &json!("targets rm")),
        "{envelope}"
    );
    assert_eq!(envelope["data"]["managed_addons"], 0);
    assert!(
        pserver.join("Interface/.omaddons/target.json").is_file(),
        "marker kept"
    );
    assert!(record.is_file(), "state kept");
    let (code, envelope) = run(&env, &["targets", "rm", "pserver"]);
    assert_eq!(
        (code, &envelope["error"]["code"]),
        (3, &json!("target_not_found"))
    );

    let (_, readded) = run(&env, &["targets", "add", &path]);
    assert_eq!(
        readded["data"]["targets"][0]["key"], key,
        "the same key comes back"
    );
}

#[test]
fn choosing_the_default() {
    let env = TestEnv::new();
    machine(&env);
    let (code, envelope) = run(&env, &["targets", "default", "era"]);
    assert_eq!(
        (code, &envelope["command"]),
        (0, &json!("targets default")),
        "{envelope}"
    );
    assert_eq!(envelope["data"]["overridden_by_config"], false);
    let (_, listed) = run(&env, &["targets"]);
    assert_eq!(listed["data"]["default"], "era");
    assert_eq!(target(&listed, "era")["default"], true);

    let config = env.config().join("omaddons/config.toml");
    fs::create_dir_all(config.parent().unwrap()).unwrap();
    fs::write(&config, "default_target = \"retail\"\n").unwrap();
    let (_, envelope) = run(&env, &["targets", "default", "era"]);
    assert_eq!(envelope["data"]["overridden_by_config"], true);
    // A selector that names the same client is no override: `sod` is played in era.
    fs::write(&config, "default_target = \"sod\"\n").unwrap();
    let (_, envelope) = run(&env, &["targets", "default", "era"]);
    assert_eq!(
        envelope["data"]["overridden_by_config"], false,
        "{envelope}"
    );
}

#[test]
fn rescan_bypasses_a_poisoned_cache() {
    let env = TestEnv::new();
    machine(&env);
    run(&env, &["targets"]);
    let cache = env.cache().join("omaddons/discovery.json");
    let poisoned = fs::read_to_string(&cache)
        .unwrap()
        .replace("12.1.5.70391", "99.0.0.1");
    fs::write(&cache, poisoned).unwrap();
    let (_, cached) = run(&env, &["targets"]);
    assert_eq!(target(&cached, "retail")["version"], "99.0.0.1");
    let (_, rescanned) = run(&env, &["targets", "--rescan"]);
    assert_eq!(
        (
            &target(&rescanned, "retail")["version"],
            &rescanned["data"]["rescanned"]
        ),
        (&json!("12.1.5.70391"), &json!(true))
    );
    assert!(!fs::read_to_string(&cache).unwrap().contains("99.0.0.1"));
}

#[test]
fn dry_runs_write_nothing() {
    let env = TestEnv::new();
    let pserver = machine(&env);
    run(&env, &["targets", "rename", "era", "keep"]);
    let path = pserver.display().to_string();
    let snapshot = || {
        (
            tree_hash(&env.home()),
            tree_hash(&env.state()),
            tree_hash(env.root().join("elsewhere").as_path()),
        )
    };
    let before = snapshot();
    for args in [
        &[
            "targets",
            "add",
            path.as_str(),
            "--name",
            "pserver",
            "--dry-run",
        ][..],
        &["targets", "rm", "keep", "--dry-run"][..],
        &["targets", "rename", "retail", "main", "--dry-run"][..],
        &["targets", "default", "retail", "--dry-run"][..],
    ] {
        let (code, envelope) = run(&env, args);
        assert_eq!(
            (code, &envelope["data"]["dry_run"]),
            (0, &json!(true)),
            "{args:?}: {envelope}"
        );
    }
    assert_eq!(snapshot(), before, "nothing written");
}

#[test]
fn a_held_lock_times_out_as_locked() {
    let env = TestEnv::new();
    machine(&env);
    let paths = paths(&env);
    let _guard = GlobalLock::acquire(&paths, "test").unwrap();
    let started = std::time::Instant::now();
    let (code, envelope) = run(&env, &["targets", "rename", "retail", "main"]);
    assert!(started.elapsed().as_secs() >= 4, "{:?}", started.elapsed());
    assert_eq!(
        (
            code,
            &envelope["error"]["code"],
            &envelope["error"]["retryable"]
        ),
        (5, &json!("locked"), &json!(true))
    );
    assert_eq!(
        envelope["error"]["fix_argv"],
        json!(["omaddons", "targets", "rename", "retail", "main"])
    );
}

/// An Era folder whose version cannot be read: a family override gives it that family's
/// current rules (game-suffix TOCs, game types), and `unknown_flavor` goes quiet.
#[test]
fn a_family_override_on_an_unreadable_client_takes_the_familys_rules() {
    let env = TestEnv::new();
    BattlenetRoot::at(Prefix::new(&env, "battlenet").wow_root())
        .product("wow")
        .unknown_folder("_classic_era_", Some("wow_classic_era"), None)
        .build();
    let addons = env.root().join("addons");
    for (path, toc) in [
        ("Multi/Multi_Mainline.toc", "## Interface: 120105\nm.lua\n"),
        ("Multi/Multi_Vanilla.toc", "## Interface: 11509\nm.lua\n"),
        (
            "Gated/Gated.toc",
            "## Interface: 11509\n## AllowLoadGameType: vanilla\ng.lua\n",
        ),
        ("Fv/Fv_Forever.toc", "## Interface: 16001\nf.lua\n"),
        ("Fv/Fv_Mainline.toc", "## Interface: 120105\nf.lua\n"),
    ] {
        let path = addons.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, toc).unwrap();
    }
    let addon = |name: &str| addons.join(name).to_string_lossy().into_owned();
    let installs = |name: &str| {
        let (code, envelope) = run(&env, &["install", &addon(name), "-t", "era", "--dry-run"]);
        assert_eq!(code, 0, "{name}: {envelope}");
        assert_eq!(
            envelope["data"]["items"][0]["status"], "installed",
            "{envelope}"
        );
    };
    let codes = |envelope: &Value| -> Vec<String> {
        envelope["warnings"]
            .as_array()
            .map(|ws| {
                ws.iter()
                    .map(|w| w["code"].as_str().unwrap().to_owned())
                    .collect()
            })
            .unwrap_or_default()
    };

    let (_, before) = run(&env, &["targets"]);
    assert_eq!(codes(&before), ["unknown_flavor"], "{before}");

    let config = env.config().join("omaddons/config.toml");
    fs::create_dir_all(config.parent().unwrap()).unwrap();
    fs::write(&config, "[targets.era]\nfamily = \"vanilla\"\n").unwrap();
    let (_, listed) = run(&env, &["targets", "-v"]);
    let era = target(&listed, "era");
    assert_eq!(
        (&era["family"], &era["details"]["rules"], &era["interface"]),
        (&json!("vanilla"), &json!("vanilla"), &json!(0)),
        "{listed}"
    );
    assert!(codes(&listed).is_empty(), "{listed}");
    // Their _Mainline TOCs (120105) would be refused: each loads its vanilla TOC.
    installs("Multi");
    installs("Gated");

    fs::write(&config, "[targets.era]\nfamily = \"forever\"\n").unwrap();
    let (_, listed) = run(&env, &["targets", "-v"]);
    assert_eq!(target(&listed, "era")["details"]["rules"], "forever");
    installs("Fv");
}

/// An Anniversary folder without `.flavor.info`, named only by `product.db`, whose uid
/// (`wow_classic_anniversary`) is not its product code: the target is
/// `wow_anniversary`'s, with its game mode, its `.build.info` version, and both names.
#[test]
fn an_anniversary_folder_named_only_by_product_db_gets_its_product_code() {
    let env = TestEnv::new();
    let prefix = Prefix::new(&env, "battlenet");
    BattlenetRoot::at(prefix.wow_root())
        .product("wow_anniversary")
        .without_flavor_info("_anniversary_")
        .build();
    common::product_db::multi_product().write(prefix.path());
    let (code, listed) = run(&env, &["targets"]);
    assert_eq!(code, 0, "{listed}");
    let anniversary = target(&listed, "anniversary");
    assert_eq!(
        (
            &anniversary["product"],
            &anniversary["found_by"],
            &anniversary["version"],
            &anniversary["modes"][0]["id"]
        ),
        (
            &json!("wow_anniversary"),
            &json!("product_db"),
            &json!("2.5.6.64702"),
            &json!("tbc-anniversary")
        ),
        "{listed}"
    );
    for selector in ["wow_anniversary", "tbc-anniversary"] {
        let (code, status) = run(&env, &["status", "-t", selector]);
        assert_eq!(code, 0, "{selector}: {status}");
    }
}

/// The human table keeps the label and the notes in columns of their own, so every
/// client's path starts in the same place.
#[test]
fn the_targets_table_aligns_paths_whatever_the_label() {
    let env = TestEnv::new();
    machine(&env);
    run(&env, &["targets", "default", "retail"]);
    let config = env.config().join("omaddons/config.toml");
    fs::create_dir_all(config.parent().unwrap()).unwrap();
    fs::write(&config, "[targets.retail]\nlabel = \"localhost\"\n").unwrap();
    let out = env.command().arg("targets").output().unwrap();
    let stdout = String::from_utf8(out.stdout).unwrap();
    let starts: Vec<usize> = stdout
        .lines()
        .filter_map(|line| line.find(" /").map(|at| line[..at].chars().count()))
        .collect();
    assert_eq!(starts.len(), 2, "{stdout}");
    assert_eq!(starts[0], starts[1], "{stdout}");
    assert!(
        stdout.contains("localhost") && stdout.contains("(unregistered)"),
        "{stdout}"
    );
}
