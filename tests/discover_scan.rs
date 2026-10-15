//! `discover()` over a synthetic home: hints, the bounded scan, de-duplication and
//! deterministic output.

mod common;

use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};

use common::env::TestEnv;
use common::procfs::running_as_root;
use common::product_db::ProductDb;
use common::trees::{BattlenetRoot, CustomRoot, Prefix, tree_hash};
use omaddons::discover::{
    Discovery, DiscoveryInput, DiscoverySource, ExtraPrefix, LauncherHint, ScanLimits, discover,
    discover_with,
};
use omaddons::error::WarningCode;
use omaddons::rules::{Family, RulesTable};

/// One row per target: (client dir relative to the env root, id, qualifier, source).
fn rows(d: &Discovery, root: &Path) -> Vec<(String, String, String, String)> {
    d.targets
        .iter()
        .map(|t| {
            let rel = t
                .target
                .client_dir
                .strip_prefix(root)
                .unwrap_or(&t.target.client_dir);
            let source = serde_json::to_value(&t.source)
                .ok()
                .and_then(|v| v["kind"].as_str().map(str::to_owned))
                .unwrap_or_default();
            (
                rel.display().to_string(),
                t.default_id.clone(),
                t.qualifier.clone(),
                source,
            )
        })
        .collect()
}

fn row(dir: &str, id: &str, qualifier: &str, source: &str) -> (String, String, String, String) {
    (
        dir.to_owned(),
        id.to_owned(),
        qualifier.to_owned(),
        source.to_owned(),
    )
}

fn input(env: &TestEnv) -> DiscoveryInput {
    DiscoveryInput {
        home: env.home(),
        ..DiscoveryInput::default()
    }
}

#[test]
fn every_route_finds_its_root() {
    let env = TestEnv::new();
    let elsewhere = env.root().join("drive-d");
    let battlenet = Prefix::new(&env, "battlenet")
        .with_drive('d', &elsewhere)
        .with_dirs(&["windows", "users/x", "ProgramData"]);
    BattlenetRoot::at(battlenet.wow_root())
        .product("wow")
        .product("wow_classic_era")
        .build();
    BattlenetRoot::at(elsewhere.join("Games/World of Warcraft"))
        .product("wowf")
        .build();
    ProductDb::new()
        .with_entry_for("agent", "C:/ProgramData/Battle.net/Agent")
        .install(
            "wow",
            "C:/Program Files (x86)/World of Warcraft",
            "_retail_",
            "12.1.5.70391",
        )
        .install(
            "wowf",
            "D:/Games/World of Warcraft",
            "_forever_",
            "1.60.1.70245",
        )
        .write(battlenet.path());
    let wotlk = Prefix::new(&env, "wow-335");
    CustomRoot::at(wotlk.drive_c().join("WoW"))
        .blizzard_folders(23)
        .build();
    CustomRoot::at(env.home().join("Games/turtle"))
        .version(1, 12, 1, 5875)
        .build();
    let extra = env.root().join("mnt/clients");
    CustomRoot::at(extra.join("TBC Client"))
        .version(2, 4, 3, 8606)
        .build();

    let before = tree_hash(env.root());
    let input = DiscoveryInput {
        extra_roots: vec![extra],
        ..input(&env)
    };
    let d = discover(&input, &RulesTable::builtin());
    let context = serde_json::to_string_pretty(&d).unwrap();
    assert_eq!(
        rows(&d, env.root()),
        [
            row(
                "drive-d/Games/World of Warcraft/_forever_",
                "forever",
                "battlenet",
                "product_db"
            ),
            row(
                "home/Games/battlenet/drive_c/Program Files (x86)/World of Warcraft/_classic_era_",
                "era",
                "battlenet",
                "product_db"
            ),
            row(
                "home/Games/battlenet/drive_c/Program Files (x86)/World of Warcraft/_retail_",
                "retail",
                "battlenet",
                "product_db"
            ),
            row("home/Games/turtle", "vanilla", "turtle", "scan"),
            row("home/Games/wow-335/drive_c/WoW", "wotlk", "wow-335", "scan"),
            row("mnt/clients/TBC Client", "tbc", "tbc-client", "extra_root"),
        ],
        "{context}"
    );
    assert!(d.warnings.is_empty(), "{context}");
    let forever = &d.targets[0].target;
    assert_eq!(
        (forever.family, forever.interface),
        (Family::Forever, 16_001)
    );
    assert_eq!(tree_hash(env.root()), before, "discovery wrote nothing");
}

#[test]
fn a_root_reachable_twice_appears_once_in_its_first_routes_terms() {
    let env = TestEnv::new();
    let prefix = Prefix::new(&env, "battlenet");
    BattlenetRoot::at(prefix.wow_root()).product("wow").build();
    ProductDb::new()
        .install(
            "wow",
            "C:/Program Files (x86)/World of Warcraft",
            "_retail_",
            "12.1.5.70391",
        )
        .write(prefix.path());
    CustomRoot::at(env.home().join("Games/wotlk")).build();
    symlink(
        env.home().join("Games/wotlk"),
        env.home().join("Games/alias"),
    )
    .unwrap();

    let d = discover(
        &DiscoveryInput {
            extra_roots: vec![env.home().join("Games/alias")],
            ..input(&env)
        },
        &RulesTable::builtin(),
    );
    let context = serde_json::to_string_pretty(&d).unwrap();
    assert_eq!(
        rows(&d, env.root()),
        [
            row("home/Games/alias", "wotlk", "alias", "extra_root"),
            row(
                "home/Games/battlenet/drive_c/Program Files (x86)/World of Warcraft/_retail_",
                "retail",
                "battlenet",
                "product_db"
            ),
        ],
        "{context}"
    );
}

#[test]
fn skip_dirs_and_depth_bound_the_scan() {
    let env = TestEnv::new();
    let prefix = Prefix::new(&env, "box");
    let c = prefix.drive_c();
    for dir in [
        "WINDOWS/wow",
        "Users/x/wow",
        "programdata/wow",
        "a/b/c/d2/e",
    ] {
        CustomRoot::at(c.join(dir)).build();
    }
    CustomRoot::at(c.join("a/b/c/d")).build();
    let d = discover(&input(&env), &RulesTable::builtin());
    assert_eq!(
        rows(&d, env.root()),
        [row(
            "home/Games/box/drive_c/a/b/c/d",
            "wotlk",
            "box",
            "scan"
        )],
        "{}",
        serde_json::to_string_pretty(&d).unwrap()
    );
}

#[test]
fn loops_permissions_and_the_budget() {
    let env = TestEnv::new();
    let games = env.home().join("Games");
    fs::create_dir_all(games.join("loop/inner")).unwrap();
    symlink("..", games.join("loop/inner/up")).unwrap();
    symlink(&games, games.join("loop/games")).unwrap();
    CustomRoot::at(games.join("wotlk")).build();
    let locked = games.join("locked");
    fs::create_dir_all(locked.join("WoW")).unwrap();
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();

    let d = discover(&input(&env), &RulesTable::builtin());
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(
        rows(&d, env.root()),
        [row("home/Games/wotlk", "wotlk", "wotlk", "scan")]
    );
    if !running_as_root() {
        assert!(
            d.diagnostics
                .iter()
                .any(|m| m.contains("cannot list") && m.contains("locked")),
            "{:?}",
            d.diagnostics
        );
    }

    let limits = ScanLimits {
        max_dirs: 2,
        ..ScanLimits::default()
    };
    let d = discover_with(&input(&env), &RulesTable::builtin(), limits);
    assert_eq!(d.warnings.len(), 1, "{:?}", d.warnings);
    assert_eq!(d.warnings[0].code, WarningCode::DiscoveryTruncated);
    assert!(
        d.warnings[0]
            .message
            .contains("after visiting 2 directories"),
        "{}",
        d.warnings[0].message
    );
}

#[test]
fn hinted_prefixes_and_exes() {
    let env = TestEnv::new();
    let bottles = env.root().join("bottles/wow");
    let bottle = Prefix::at(bottles.clone());
    CustomRoot::at(bottle.drive_c().join("Games/Cata"))
        .version(4, 3, 4, 15595)
        .build();
    let lutris_exe_root = env.root().join("lutris-games/mop");
    CustomRoot::at(lutris_exe_root.clone())
        .version(5, 4, 8, 18414)
        .build();
    let flavor = BattlenetRoot::at(env.root().join("heroic/World of Warcraft"))
        .product("wow_anniversary")
        .build();
    let process = Prefix::at(env.root().join("running/pfx"));
    CustomRoot::at(process.drive_c().join("WoW")).build();

    let input = DiscoveryInput {
        launcher_hints: vec![
            LauncherHint {
                launcher: "bottles".into(),
                flatpak: false,
                prefix: Some(bottles),
                exe: None,
            },
            LauncherHint {
                launcher: "lutris".into(),
                flatpak: true,
                prefix: None,
                exe: Some(lutris_exe_root.join("Wow.exe")),
            },
            LauncherHint {
                launcher: "heroic".into(),
                flatpak: false,
                prefix: None,
                exe: Some(flavor.join("_anniversary_/WoWClassic.exe")),
            },
            LauncherHint {
                launcher: "lutris".into(),
                flatpak: false,
                prefix: Some(env.root().join("not-a-prefix")),
                exe: Some(env.root().join("nowhere/Wow.exe")),
            },
        ],
        extra_prefixes: vec![ExtraPrefix {
            path: process.path().to_path_buf(),
            qualifier: "pfx".into(),
            source: DiscoverySource::Process,
        }],
        ..input(&env)
    };
    let d = discover(&input, &RulesTable::builtin());
    let context = serde_json::to_string_pretty(&d).unwrap();
    assert_eq!(
        rows(&d, env.root()),
        [
            row(
                "bottles/wow/drive_c/Games/Cata",
                "cata",
                "bottles",
                "launcher"
            ),
            row(
                "heroic/World of Warcraft/_anniversary_",
                "anniversary",
                "heroic",
                "launcher"
            ),
            row("lutris-games/mop", "mists", "lutris", "launcher"),
            row("running/pfx/drive_c/WoW", "wotlk", "pfx", "process"),
        ],
        "{context}"
    );
    assert!(
        d.diagnostics
            .iter()
            .any(|m| m.contains("not a Wine prefix")),
        "{context}"
    );
    assert!(
        d.diagnostics
            .iter()
            .any(|m| m.contains("not in a WoW root")),
        "{context}"
    );
}

#[test]
fn output_is_deterministic() {
    let env = TestEnv::new();
    let prefix = Prefix::new(&env, "battlenet");
    BattlenetRoot::at(prefix.wow_root())
        .all_products()
        .unknown_folder("_xyz_", None, None)
        .last_addon_version("_retail_", 120_100)
        .build();
    for name in ["b", "a", "c"] {
        CustomRoot::at(env.home().join("Games").join(name)).build();
    }
    let run = || serde_json::to_string(&discover(&input(&env), &RulesTable::builtin())).unwrap();
    let first = run();
    assert_eq!(first, run());
    let d = discover(&input(&env), &RulesTable::builtin());
    assert_eq!(d.targets.len(), 16);
    let codes: Vec<&str> = d.warnings.iter().map(|w| w.code.wire()).collect();
    assert_eq!(codes, ["client_not_launched_since_patch", "unknown_flavor"]);
}

#[test]
fn an_extra_root_at_a_flavor_folder_finds_its_battlenet_root() {
    let env = TestEnv::new();
    let root = BattlenetRoot::at(env.root().join("bnet"))
        .product("wow")
        .product("wowf")
        .build();
    let input = DiscoveryInput {
        extra_roots: vec![root.join("_forever_")],
        ..input(&env)
    };
    let d = discover(&input, &RulesTable::builtin());
    assert_eq!(
        rows(&d, env.root()),
        [
            row("bnet/_forever_", "forever", "bnet", "extra_root"),
            row("bnet/_retail_", "retail", "bnet", "extra_root"),
        ],
        "{}",
        serde_json::to_string_pretty(&d).unwrap()
    );
}

#[test]
fn an_empty_home_finds_nothing() {
    let env = TestEnv::new();
    let d = discover(&input(&env), &RulesTable::builtin());
    assert!(d.targets.is_empty() && d.warnings.is_empty(), "{d:?}");
    let missing = DiscoveryInput {
        home: PathBuf::from("/nonexistent-home-for-omaddons-tests"),
        ..DiscoveryInput::default()
    };
    assert!(
        discover(&missing, &RulesTable::builtin())
            .targets
            .is_empty()
    );
}
