//! Launcher configs as discovery hints: Lutris, Heroic, Steam and Bottles, native and
//! Flatpak, over a synthetic home whose configs point at prefixes and clients outside
//! the bounded scan. Malformed configs never fail discovery, and nothing is written.

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::env::TestEnv;
use common::launchers::{
    Install, LutrisDir, Malformed, bottle, heroic_game, lutris_game, malform, steam_libraries,
    steam_prefix, steam_root,
};
use common::trees::{BattlenetRoot, CustomRoot, Prefix, tree_stat};
use omaddons::discover::launchers::{LauncherDirs, collect_hints};
use omaddons::discover::{Discovery, DiscoveryInput, LauncherHint, discover};
use omaddons::rules::RulesTable;
use serde_json::Value;
use support::{dirs, input, machine, prefixes, row, rows};

#[cfg(test)]
mod support {
    use super::*;

    pub fn dirs(env: &TestEnv) -> LauncherDirs {
        LauncherDirs {
            home: env.home(),
            config: env.config(),
            data: env.data(),
        }
    }

    pub fn input(env: &TestEnv) -> DiscoveryInput {
        DiscoveryInput {
            home: env.home(),
            config_home: Some(env.config()),
            data_home: Some(env.data()),
            ..DiscoveryInput::default()
        }
    }

    /// (client dir relative to the env root, id, qualifier, source name), sorted.
    pub fn rows(d: &Discovery, root: &Path) -> Vec<(String, String, String, String)> {
        let mut rows: Vec<_> = d
            .targets
            .iter()
            .map(|t| {
                let rel = t.target.client_dir.strip_prefix(root).unwrap();
                let source = serde_json::to_value(&t.source).unwrap();
                (
                    rel.display().to_string(),
                    t.default_id.clone(),
                    t.qualifier.clone(),
                    source["name"].as_str().unwrap_or("").to_owned(),
                )
            })
            .collect();
        rows.sort();
        rows
    }

    pub fn row(dir: &str, id: &str, qualifier: &str) -> (String, String, String, String) {
        (dir.into(), id.into(), qualifier.into(), qualifier.into())
    }

    /// One config per launcher and install kind, each pointing at a client.
    pub fn machine(env: &TestEnv) {
        let root = env.root();
        // Lutris, <config>: an exe outside any prefix, and a prefix.
        CustomRoot::at(root.join("lutris-games/mop"))
            .version(5, 4, 8, 18414)
            .build();
        let pfx = Prefix::at(root.join("lutris-pfx"));
        CustomRoot::at(pfx.drive_c().join("Games/Cata"))
            .version(4, 3, 4, 15595)
            .build();
        lutris_game(
            env,
            LutrisDir::Config,
            "wow-mop",
            Some(&root.join("lutris-games/mop/Wow.exe")),
            Some(pfx.path()),
        );
        // Lutris, <data>: double quotes and CRLF, a non-WoW exe.
        let pfx = Prefix::at(root.join("lutris-data-pfx"));
        BattlenetRoot::at(pfx.wow_root()).product("wow").build();
        let yml = env.data().join("lutris/games/wow-retail.yml");
        fs::create_dir_all(yml.parent().unwrap()).unwrap();
        fs::write(
            &yml,
            format!(
                "game:\r\n  exe: \"{}\"\r\n  prefix: \"{}\"\r\nname: wow\r\n",
                pfx.path().join("drive_c/other/Game.exe").display(),
                pfx.path().display()
            ),
        )
        .unwrap();
        // Heroic, native and Flatpak.
        let pfx = Prefix::at(root.join("heroic-pfx"));
        CustomRoot::at(pfx.drive_c().join("Games/WotLK")).build();
        heroic_game(env, Install::Native, "wow335", pfx.path());
        let pfx = Prefix::at(root.join("heroic-flatpak-pfx"));
        BattlenetRoot::at(pfx.wow_root())
            .product("wow_classic_era")
            .build();
        heroic_game(env, Install::Flatpak, "wowera", pfx.path());
        // Steam, native: the root and a library on another disk.
        let disk = root.join("disk2/SteamLibrary");
        steam_libraries(env, Install::Native, &[&disk]);
        steam_prefix(&steam_root(env, Install::Native), 1);
        let pfx = steam_prefix(&disk, 2_147_483_649);
        BattlenetRoot::at(pfx.join("drive_c/Program Files (x86)/World of Warcraft"))
            .product("wow_anniversary")
            .build();
        // Steam, Flatpak.
        steam_libraries(env, Install::Flatpak, &[]);
        let pfx = steam_prefix(&steam_root(env, Install::Flatpak), 7);
        CustomRoot::at(pfx.join("drive_c/Games/Cata"))
            .version(4, 3, 4, 15595)
            .build();
        // Bottles: two native bottles, one Flatpak.
        let wow = bottle(env, Install::Native, "wow", None);
        CustomRoot::at(wow.join("drive_c/Games/MoP"))
            .version(5, 4, 8, 18414)
            .build();
        bottle(env, Install::Native, "other", None);
        let classic = bottle(env, Install::Flatpak, "classic", None);
        CustomRoot::at(classic.join("drive_c/Games/Classic")).build();
    }

    pub fn prefixes(hints: &[LauncherHint], launcher: &str, flatpak: bool) -> Vec<PathBuf> {
        hints
            .iter()
            .filter(|h| h.launcher == launcher && h.flatpak == flatpak)
            .filter_map(|h| h.prefix.clone())
            .collect()
    }
}

#[test]
fn every_launcher_config_leads_to_its_clients() {
    let env = TestEnv::new();
    machine(&env);
    let root = env.root();
    let (hints, diagnostics) = collect_hints(&dirs(&env));
    let context = format!("{hints:#?}\n{diagnostics:#?}");
    assert!(diagnostics.is_empty(), "{context}");
    assert_eq!(
        prefixes(&hints, "lutris", false),
        [root.join("lutris-pfx"), root.join("lutris-data-pfx")],
        "{context}"
    );
    let exes: Vec<&PathBuf> = hints.iter().filter_map(|h| h.exe.as_ref()).collect();
    assert_eq!(exes, [&root.join("lutris-games/mop/Wow.exe")], "{context}");
    assert_eq!(prefixes(&hints, "heroic", false), [root.join("heroic-pfx")]);
    assert_eq!(
        prefixes(&hints, "heroic", true),
        [root.join("heroic-flatpak-pfx")]
    );
    assert_eq!(
        prefixes(&hints, "steam", false),
        [
            steam_root(&env, Install::Native).join("steamapps/compatdata/1/pfx"),
            root.join("disk2/SteamLibrary/steamapps/compatdata/2147483649/pfx"),
        ],
        "{context}"
    );
    assert_eq!(
        prefixes(&hints, "steam", true),
        [steam_root(&env, Install::Flatpak).join("steamapps/compatdata/7/pfx")]
    );
    assert_eq!(prefixes(&hints, "bottles", false).len(), 2, "{context}");
    assert_eq!(prefixes(&hints, "bottles", true).len(), 1, "{context}");

    let d = discover(&input(&env), &RulesTable::builtin());
    let context = serde_json::to_string_pretty(&d).unwrap();
    let steam_flatpak = "home/.var/app/com.valvesoftware.Steam/.local/share/Steam";
    let bottles_flatpak = "home/.var/app/com.usebottles.bottles/data/bottles/bottles";
    let mut expected = vec![
        row("lutris-games/mop", "mists", "lutris"),
        row("lutris-pfx/drive_c/Games/Cata", "cata", "lutris"),
        row(
            "lutris-data-pfx/drive_c/Program Files (x86)/World of Warcraft/_retail_",
            "retail",
            "lutris",
        ),
        row("heroic-pfx/drive_c/Games/WotLK", "wotlk", "heroic"),
        row(
            "heroic-flatpak-pfx/drive_c/Program Files (x86)/World of Warcraft/_classic_era_",
            "era",
            "heroic",
        ),
        row(
            "disk2/SteamLibrary/steamapps/compatdata/2147483649/pfx/drive_c/Program Files (x86)/World of Warcraft/_anniversary_",
            "anniversary",
            "steam",
        ),
        row(
            &format!("{steam_flatpak}/steamapps/compatdata/7/pfx/drive_c/Games/Cata"),
            "cata",
            "steam",
        ),
        row(
            "home/.local/share/bottles/bottles/wow/drive_c/Games/MoP",
            "mists",
            "bottles",
        ),
        row(
            &format!("{bottles_flatpak}/classic/drive_c/Games/Classic"),
            "wotlk",
            "bottles",
        ),
    ];
    expected.sort();
    assert_eq!(rows(&d, root), expected, "{context}");
    assert!(
        d.diagnostics.iter().all(|m| !m.contains("Game.exe")),
        "a non-WoW exe is not a hint: {context}"
    );
}

#[test]
fn the_steam_links_and_a_repeated_library_are_read_once() {
    let env = TestEnv::new();
    let native = steam_root(&env, Install::Native);
    // The vdf lists the root itself again, through the ~/.steam link.
    fs::create_dir_all(env.home().join(".steam")).unwrap();
    std::os::unix::fs::symlink(&native, env.home().join(".steam/steam")).unwrap();
    steam_libraries(&env, Install::Native, &[&env.home().join(".steam/steam")]);
    let pfx = steam_prefix(&native, 9);
    let (hints, _) = collect_hints(&dirs(&env));
    assert_eq!(prefixes(&hints, "steam", false), [pfx], "{hints:#?}");
}

#[test]
fn malformed_configs_never_fail_discovery_and_nothing_is_written() {
    for how in [Malformed::Truncated, Malformed::Binary, Malformed::Empty] {
        let env = TestEnv::new();
        machine(&env);
        let files = [
            env.config().join("lutris/games/wow-mop.yml"),
            env.data().join("lutris/games/wow-retail.yml"),
            env.config().join("heroic/GamesConfig/wow335.json"),
            steam_root(&env, Install::Native).join("steamapps/libraryfolders.vdf"),
            env.data().join("bottles/bottles/wow/bottle.yml"),
        ];
        for file in &files {
            malform(file, how);
        }
        let before = tree_stat(env.root());
        let d = discover(&input(&env), &RulesTable::builtin());
        assert_eq!(tree_stat(env.root()), before, "{how:?}: discovery wrote");
        let context = serde_json::to_string_pretty(&d).unwrap();
        assert!(
            d.diagnostics
                .iter()
                .any(|m| m.contains("wow335.json is not valid JSON")),
            "{how:?}: {context}"
        );
        // The prefix-only routes that survive: Heroic Flatpak, the Steam root, Bottles.
        let found: Vec<String> = rows(&d, env.root()).into_iter().map(|r| r.3).collect();
        for launcher in ["heroic", "steam", "bottles"] {
            assert!(found.iter().any(|l| l == launcher), "{how:?}: {context}");
        }
    }
}

#[test]
fn the_binary_reads_launcher_configs_under_the_xdg_directories() {
    let env = TestEnv::new();
    CustomRoot::at(env.root().join("elsewhere/mop"))
        .version(5, 4, 8, 18414)
        .build();
    // <config> lies outside the home here, so only XDG_CONFIG_HOME finds it.
    lutris_game(
        &env,
        LutrisDir::Config,
        "mop",
        Some(&env.root().join("elsewhere/mop/Wow.exe")),
        None,
    );
    let output = env.command().args(["targets", "--json"]).output().unwrap();
    let envelope: Value = serde_json::from_slice(&output.stdout).unwrap();
    let targets = envelope["data"]["targets"].as_array().unwrap();
    assert_eq!(
        targets
            .iter()
            .map(|t| (t["id"].as_str().unwrap(), t["found_by"].as_str().unwrap()))
            .collect::<Vec<_>>(),
        [("mists", "launcher:lutris")],
        "{envelope:#}"
    );
}
