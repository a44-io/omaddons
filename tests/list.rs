//! `omaddons list` end to end: the binary over a synthetic home with registered
//! targets and records written as install will write them.

mod common;

use std::fs;
use std::os::unix::fs::symlink;
use std::path::Path;

use common::addons::{addon_folder, toc};
use common::env::TestEnv;
use common::trees::{BattlenetRoot, CustomRoot, Prefix, tree_hash};
use omaddons::discover::registry::{LockedRegistry, reconcile};
use omaddons::discover::{DiscoveryInput, discover};
use omaddons::fsx::{GlobalLock, ScopeTable, WriteSession, Writer};
use omaddons::model::TargetKey;
use omaddons::rules::RulesTable;
use omaddons::state::paths::Paths;
use omaddons::state::records::TargetRecord;
use omaddons::state::registry::load_registry;
use serde_json::Value;
use support::{list, retail};

#[cfg(test)]
mod support {
    use super::*;
    use omaddons::model::{
        AddonReason, AddonRecord, AddonState, Channel, FolderName, VersionConfidence,
    };

    pub fn paths(env: &TestEnv) -> Paths {
        Paths::with_roots(env.home(), env.config(), env.state(), env.cache())
    }

    /// Registers every discovered target; returns id → key.
    pub fn register_all(env: &TestEnv) -> Vec<(String, TargetKey)> {
        let paths = paths(env);
        let table = RulesTable::builtin();
        let input = DiscoveryInput {
            home: env.home(),
            ..DiscoveryInput::default()
        };
        let found = discover(&input, &table);
        let resolved = reconcile(&load_registry(&paths).unwrap().value, &found.targets);
        let lock = GlobalLock::acquire(&paths, "test").unwrap();
        let dirs: Vec<_> = resolved
            .targets
            .iter()
            .map(|t| t.target.client_dir.clone())
            .collect();
        let scopes = ScopeTable::new(&paths, &dirs, None).unwrap();
        let mut s = WriteSession::untracked(Writer::Mutating, &scopes, "test");
        let mut registry = LockedRegistry::load(&lock, &paths).unwrap();
        resolved
            .targets
            .iter()
            .map(|t| {
                (
                    t.target.id.clone(),
                    registry.ensure_registered(&mut s, t).unwrap(),
                )
            })
            .collect()
    }

    /// Writes `<state>/omaddons/targets/<key>.json` with addons (name, version, ref,
    /// folders).
    pub fn write_record(env: &TestEnv, key: TargetKey, addons: &[(&str, &str, &str, &[&str])]) {
        let mut record = TargetRecord::empty(key);
        for (name, version, source, folders) in addons {
            record.addons.insert(
                TargetRecord::addon_key(name),
                AddonRecord {
                    r#ref: source.parse().unwrap(),
                    name: (*name).into(),
                    version: (*version).into(),
                    version_confidence: VersionConfidence::Release,
                    release_id: None,
                    channel: Channel::Stable,
                    pinned: false,
                    reason: AddonReason::Explicit,
                    required_by: vec![],
                    folders: folders
                        .iter()
                        .map(|f| FolderName::new(f).unwrap())
                        .collect(),
                    state: AddonState::Ok,
                    installed_at: "2026-10-14T21:00:00Z".parse().unwrap(),
                    tx_id: "01JB8Q2VA00000000000000000".parse().unwrap(),
                    blob_sha256: None,
                    revision: None,
                },
            );
        }
        let path = paths(env).target_record(&key);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, omaddons::state::store::encode(&record).unwrap()).unwrap();
    }

    /// Runs `omaddons list <args> --json` and returns (exit code, envelope).
    pub fn list(env: &TestEnv, args: &[&str]) -> (i32, Value) {
        let output = env
            .command()
            .arg("list")
            .args(args)
            .arg("--json")
            .output()
            .unwrap();
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert_eq!(stdout.lines().count(), 1, "one envelope\n{stdout}");
        (
            output.status.code().unwrap(),
            serde_json::from_str(&stdout).unwrap(),
        )
    }

    /// A retail target with WeakAuras (two folders) and Details installed, one
    /// unmanaged folder and one linked folder; returns its AddOns dir.
    pub fn retail(env: &TestEnv) -> std::path::PathBuf {
        let prefix = Prefix::new(env, "battlenet");
        let root = BattlenetRoot::at(prefix.wow_root()).product("wow").build();
        let addons = root.join("_retail_/Interface/AddOns");
        let t = toc("120105", &["## Title: X"]);
        addon_folder(&addons, "WeakAuras", &[("WeakAuras.toc", &t)], &[]);
        addon_folder(
            &addons,
            "WeakAurasOptions",
            &[("WeakAurasOptions.toc", &t)],
            &[],
        );
        addon_folder(&addons, "Details", &[("Details.toc", &t)], &[]);
        addon_folder(
            &addons,
            "OldAddon",
            &[(
                "OldAddon.toc",
                &toc("120000", &["## Title: Old Addon", "## Version: 0.9"]),
            )],
            &[],
        );
        let working_copy = env.root().join("dev/MyAddon");
        addon_folder(&working_copy, "", &[], &[]);
        symlink(&working_copy, addons.join("MyAddon")).unwrap();
        let keys = register_all(env);
        write_record(
            env,
            keys[0].1,
            &[
                (
                    "WeakAuras",
                    "5.19.10",
                    "gh:WeakAuras/WeakAuras2",
                    &["WeakAuras", "WeakAurasOptions"],
                ),
                (
                    "Details",
                    "2.3.1",
                    "gh:Tercioo/Details-Damage-Meter",
                    &["Details"],
                ),
            ],
        );
        addons
    }
}

fn target<'a>(envelope: &'a Value, id: &str) -> &'a Value {
    envelope["data"]["targets"]
        .as_array()
        .and_then(|ts| ts.iter().find(|t| t["id"] == id))
        .unwrap_or(&Value::Null)
}

#[test]
fn managed_addons_and_counts() {
    let env = TestEnv::new();
    retail(&env);
    let (code, envelope) = list(&env, &[]);
    assert_eq!(code, 0, "{envelope}");
    let t = target(&envelope, "retail");
    let names: Vec<&str> = t["addons"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["Details", "WeakAuras"], "sorted by name");
    let wa = &t["addons"][1];
    assert_eq!(
        (&wa["version"], &wa["ref"], &wa["channel"]),
        (
            &Value::from("5.19.10"),
            &Value::from("gh:WeakAuras/WeakAuras2"),
            &Value::from("stable")
        )
    );
    assert_eq!(wa["folders"], 2);
    assert_eq!(
        t["counts"],
        serde_json::json!({"managed": 3, "unmanaged": 1, "shared": 0, "protected": 0, "linked": 1, "invalid": 0})
    );
    assert!(t["unmanaged"].is_null() && t["protected"].is_null());
    assert!(t["key"].is_string());
}

#[test]
fn unmanaged_folders_carry_their_toc() {
    let env = TestEnv::new();
    retail(&env);
    let (_, envelope) = list(&env, &["--unmanaged"]);
    let unmanaged = target(&envelope, "retail")["unmanaged"]
        .as_array()
        .unwrap()
        .clone();
    let old = unmanaged
        .iter()
        .find(|f| f["folder"] == "OldAddon")
        .unwrap();
    assert_eq!(
        (
            &old["class"],
            &old["title"],
            &old["version"],
            &old["interface"],
            &old["verdict"]
        ),
        (
            &Value::from("unmanaged"),
            &Value::from("Old Addon"),
            &Value::from("0.9"),
            &Value::from(120_000),
            &Value::from("out_of_date")
        )
    );
    let linked = unmanaged.iter().find(|f| f["folder"] == "MyAddon").unwrap();
    assert_eq!(linked["class"], "linked");
    assert!(linked["verdict"].is_null(), "links are never read");
}

/// Retail 12.0 and later load no Mainline TOC below 120000, whatever the player
/// enables: such a folder on disk does not load, and nothing suggests enabling
/// out-of-date addons for it.
#[test]
fn an_unmanaged_folder_below_the_client_floor_does_not_load() {
    let env = TestEnv::new();
    let addons = retail(&env);
    addon_folder(
        &addons,
        "Ancient",
        &[("Ancient.toc", &toc("110207", &["## Title: Ancient"]))],
        &[],
    );
    let (code, envelope) = list(&env, &["--unmanaged"]);
    assert_eq!(code, 0, "{envelope}");
    let ancient = target(&envelope, "retail")["unmanaged"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["folder"] == "Ancient")
        .cloned()
        .unwrap();
    assert_eq!(ancient["verdict"], "wrong_flavor", "{ancient}");
    assert!(
        !envelope.to_string().contains("Load out of date"),
        "{envelope}"
    );
    let human = env
        .command()
        .args(["list", "--unmanaged"])
        .output()
        .unwrap();
    let text = String::from_utf8(human.stdout).unwrap();
    assert!(
        text.lines()
            .any(|l| l.contains("Ancient") && l.contains("wrong_flavor")),
        "{text}"
    );
}

#[test]
fn protected_folders_on_a_private_server_client() {
    let env = TestEnv::new();
    let root = CustomRoot::at(env.home().join("Games/wotlk"))
        .blizzard_folders(23)
        .build();
    fs::create_dir_all(root.join("Interface/AddOns/blizzard_custom")).unwrap();
    let (code, envelope) = list(&env, &["--protected"]);
    assert_eq!(code, 0);
    let t = target(&envelope, "wotlk");
    assert_eq!(t["protected"].as_array().unwrap().len(), 24, "{t}");
    assert_eq!(t["counts"]["protected"], 24);
    assert!(t["key"].is_null(), "unregistered targets are listed too");
}

#[test]
fn duplicates_and_missing_folders() {
    let env = TestEnv::new();
    let addons = retail(&env);
    addon_folder(&addons, "Foo", &[], &[]);
    addon_folder(&addons, "foo", &[], &[]);
    fs::remove_dir_all(addons.join("WeakAurasOptions")).unwrap();
    let (_, envelope) = list(&env, &["--unmanaged"]);
    let t = target(&envelope, "retail");
    let foos: Vec<&Value> = t["unmanaged"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|f| f["folder"].as_str().unwrap().eq_ignore_ascii_case("foo"))
        .collect();
    assert_eq!(foos.len(), 2);
    assert!(foos.iter().all(|f| f["case_duplicate"] == true));
    let wa = t["addons"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["name"] == "WeakAuras")
        .unwrap();
    assert_eq!(
        wa["missing_folders"],
        serde_json::json!(["WeakAurasOptions"])
    );
}

#[test]
fn verbose_lists_folders_and_paths() {
    let env = TestEnv::new();
    retail(&env);
    let output = env
        .command()
        .args(["list", "--json", "-v"])
        .output()
        .unwrap();
    let envelope: Value = serde_json::from_slice(&output.stdout).unwrap();
    let t = target(&envelope, "retail");
    assert!(t["client_dir"].as_str().unwrap().ends_with("_retail_"));
    let wa = t["addons"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["name"] == "WeakAuras")
        .unwrap();
    assert_eq!(
        wa["folders"],
        serde_json::json!(["WeakAuras", "WeakAurasOptions"])
    );
    assert_eq!(wa["folder_tocs"][0]["verdict"], "compatible");

    let human = env.command().arg("list").output().unwrap();
    let text = String::from_utf8(human.stdout).unwrap();
    assert!(
        text.starts_with("retail (12.1.5.70391, mainline)\n"),
        "{text}"
    );
    assert!(
        text.contains("WeakAuras")
            && text.contains("3 managed · 1 unmanaged · 0 protected · 1 linked"),
        "{text}"
    );
}

#[test]
/// Only the derived discovery cache may be written (any command may refresh it); state,
/// config and client dirs are untouched.
fn list_writes_nothing() {
    let env = TestEnv::new();
    retail(&env);
    let hashes = || {
        (
            tree_hash(&env.home()),
            tree_hash(&env.state()),
            tree_hash(&env.config()),
        )
    };
    let before = hashes();
    for args in [&[][..], &["--unmanaged", "--protected"][..], &["-v"][..]] {
        assert_eq!(list(&env, args).0, 0);
    }
    assert_eq!(hashes(), before, "state, config and client dirs unchanged");
    assert!(!Path::new(&env.state().join("omaddons/net.log")).exists());
    assert!(
        env.cache().join("omaddons/discovery.json").is_file(),
        "the cache is derived data"
    );
}

#[test]
/// A leftover journal is reported, never recovered, by a read command.
fn a_pending_journal_is_reported_not_recovered() {
    let env = TestEnv::new();
    retail(&env);
    let tx = omaddons::model::TxId::new();
    let journal = support::paths(&env).journal(&tx);
    fs::create_dir_all(journal.parent().unwrap()).unwrap();
    fs::write(&journal, "{\"type\":\"swapped\"}\n").unwrap();
    let before = tree_hash(&env.state());
    let (code, envelope) = list(&env, &[]);
    assert_eq!(code, 0);
    let warnings = envelope["warnings"].as_array().unwrap();
    let pending = warnings.iter().find(|w| w["code"] == "journal_pending");
    assert!(pending.is_some(), "{warnings:?}");
    let pending = pending.unwrap();
    let message = pending["message"].as_str().unwrap();
    assert!(message.contains(&tx.to_string()), "{message}");
    // A `swapped` line without a `prepared` one is a journal no build can tell the
    // story of (M3-D45): left for manual repair, never taken for "nothing ran".
    assert!(message.contains("journal_unreadable"), "{message}");
    assert_eq!(pending["fix_argv"], serde_json::Value::Null);
    assert!(journal.exists(), "readers never recover");
    assert_eq!(tree_hash(&env.state()), before);
}

#[test]
fn an_unknown_target_is_not_found() {
    let env = TestEnv::new();
    retail(&env);
    let (code, envelope) = list(&env, &["-t", "nowhere"]);
    assert_eq!(code, 3);
    assert_eq!(envelope["error"]["code"], "target_not_found");
    assert_eq!(
        envelope["error"]["fix_argv"],
        serde_json::json!(["omaddons", "targets"])
    );
    let (code, envelope) = list(&env, &["-t", "retail"]);
    assert_eq!(
        (code, envelope["data"]["targets"].as_array().map(Vec::len)),
        (0, Some(1))
    );
}
