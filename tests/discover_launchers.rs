//! Launcher configs as discovery hints: Lutris, Heroic, Steam and Bottles, native and
//! Flatpak, over a synthetic home whose configs point at prefixes and clients outside
//! the bounded scan. Malformed configs never fail discovery, and nothing is written.

use crate::common;

use std::fs;
use std::path::{Path, PathBuf};

use common::env::TestEnv;
use common::launchers::{
    Install, LutrisDir, Malformed, bottle, bottle_in, bottles_data, bottles_dir,
    custom_path_bottle, heroic_game, heroic_settings, heroic_sideloaded, lutris_game, malform,
    steam_libraries, steam_prefix, steam_root, steam_shortcuts,
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
        let wow = bottle(env, Install::Native, "wow");
        CustomRoot::at(wow.join("drive_c/Games/MoP"))
            .version(5, 4, 8, 18414)
            .build();
        bottle(env, Install::Native, "other");
        let classic = bottle(env, Install::Flatpak, "classic");
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

/// Bottles 67.4's two other homes for a bottle, native and Flatpak: a bottles directory
/// moved in its preferences (`data.yml`), and a bottle created at a custom path (only a
/// placeholder in the default directory). Only Bottles leads to them.
#[test]
fn bottles_outside_the_default_directory_are_found() {
    for install in [Install::Native, Install::Flatpak] {
        let env = TestEnv::new();
        let root = env.root();
        let moved = root.join("disk2/Bottles");
        bottles_data(&env, install, &moved);
        let wow = bottle_in(&moved, "wow");
        CustomRoot::at(wow.join("drive_c/Games/WotLK")).build();
        let custom = root.join("elsewhere/wow2");
        custom_path_bottle(&env, install, "wow2", &custom);
        CustomRoot::at(custom.join("drive_c/Games/MoP"))
            .version(5, 4, 8, 18414)
            .build();
        // Left in the default directory from before the move.
        let left = bottle(&env, install, "wow3");
        CustomRoot::at(left.join("drive_c/Games/Cata"))
            .version(4, 3, 4, 15595)
            .build();

        let flatpak = matches!(install, Install::Flatpak);
        let (hints, diagnostics) = collect_hints(&dirs(&env));
        let context = format!("{install:?}\n{hints:#?}\n{diagnostics:#?}");
        assert!(diagnostics.is_empty(), "{context}");
        assert_eq!(
            prefixes(&hints, "bottles", flatpak),
            [custom.clone(), left.clone(), wow.clone()],
            "{context}"
        );
        assert!(
            prefixes(&hints, "bottles", !flatpak).is_empty(),
            "{context}"
        );

        let d = discover(&input(&env), &RulesTable::builtin());
        let default = bottles_dir(&env, install);
        let default = default.strip_prefix(root).unwrap().display();
        let mut expected = vec![
            row("disk2/Bottles/wow/drive_c/Games/WotLK", "wotlk", "bottles"),
            row("elsewhere/wow2/drive_c/Games/MoP", "mists", "bottles"),
            row(
                &format!("{default}/wow3/drive_c/Games/Cata"),
                "cata",
                "bottles",
            ),
        ];
        expected.sort();
        assert_eq!(
            rows(&d, root),
            expected,
            "{install:?}\n{}",
            serde_json::to_string_pretty(&d).unwrap()
        );
    }
}

/// A moved bottles directory in the document portal is refused, as Bottles refuses it,
/// with a diagnostic; a placeholder naming a bottle since deleted is skipped silently.
#[test]
fn a_document_portal_bottles_directory_and_a_dangling_placeholder_lead_nowhere() {
    let env = TestEnv::new();
    let portal = Path::new("/run/user/1000/doc/1a2b3c/Bottles");
    bottles_data(&env, Install::Native, portal);
    let gone = env.root().join("gone");
    let placeholder = bottles_dir(&env, Install::Native).join("gone/placeholder.yml");
    fs::create_dir_all(placeholder.parent().unwrap()).unwrap();
    fs::write(&placeholder, format!("Path: {}\n", gone.display())).unwrap();
    let (hints, diagnostics) = collect_hints(&dirs(&env));
    assert!(hints.is_empty(), "{hints:#?}");
    assert_eq!(
        diagnostics,
        [format!(
            "bottles: custom_bottles_path {} is a document-portal path, which Bottles does not use",
            portal.display()
        )]
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

/// Heroic v2.22.4's other homes for a prefix or a client: the folder of per-game
/// prefixes (`~/Games/Heroic/Prefixes` by default, else `config.json`'s
/// `defaultWinePrefixDir`), the shared prefix, a Proton game's `pfx`, and a sideloaded
/// WoW exe; and Lutris's game files inside its Flatpak.
#[test]
fn heroic_prefix_folders_proton_sideloads_and_flatpak_lutris_lead_to_their_clients() {
    let env = TestEnv::new();
    let (root, home) = (env.root(), env.home());
    // Native Heroic: the default folder, a Proton game, a sideloaded client.
    let titled = Prefix::at(home.join("Games/Heroic/Prefixes/World of Warcraft"));
    BattlenetRoot::at(titled.wow_root()).product("wow").build();
    fs::create_dir_all(home.join("Games/Heroic/Prefixes/not-a-prefix")).unwrap();
    let proton = Prefix::at(root.join("heroic-proton/pfx"));
    CustomRoot::at(proton.drive_c().join("Games/Cata"))
        .version(4, 3, 4, 15595)
        .build();
    heroic_game(&env, Install::Native, "cata", &root.join("heroic-proton"));
    CustomRoot::at(root.join("heroic-sideload/wotlk")).build();
    heroic_sideloaded(
        &env,
        Install::Native,
        &[
            &root.join("heroic-sideload/wotlk/Wow.exe"),
            Path::new("/usr/bin/firefox"),
        ],
    );
    // Flatpak Heroic: its own folder of prefixes and a shared prefix, in config.json.
    let folder = root.join("heroic-flatpak-prefixes");
    let era = Prefix::at(folder.join("Era"));
    BattlenetRoot::at(era.wow_root())
        .product("wow_classic_era")
        .build();
    let shared = Prefix::at(root.join("heroic-shared"));
    heroic_settings(&env, Install::Flatpak, Some(shared.path()), Some(&folder));
    // Lutris's Flatpak: a prefix in its config directory, an exe in its data directory.
    let pfx = Prefix::at(root.join("lutris-flatpak-pfx"));
    CustomRoot::at(pfx.drive_c().join("Games/MoP"))
        .version(5, 4, 8, 18414)
        .build();
    lutris_game(
        &env,
        LutrisDir::FlatpakConfig,
        "wow-mop",
        None,
        Some(pfx.path()),
    );
    CustomRoot::at(root.join("lutris-flatpak-games/cata"))
        .version(4, 3, 4, 15595)
        .build();
    lutris_game(
        &env,
        LutrisDir::FlatpakData,
        "wow-cata",
        Some(&root.join("lutris-flatpak-games/cata/Wow.exe")),
        None,
    );

    let (hints, diagnostics) = collect_hints(&dirs(&env));
    let context = format!("{hints:#?}\n{diagnostics:#?}");
    assert!(diagnostics.is_empty(), "{context}");
    assert_eq!(
        prefixes(&hints, "heroic", false),
        [
            root.join("heroic-proton/pfx"),
            home.join("Games/Heroic/Prefixes/World of Warcraft"),
        ],
        "{context}"
    );
    assert_eq!(
        prefixes(&hints, "heroic", true),
        [root.join("heroic-shared"), folder.join("Era")],
        "{context}"
    );
    assert_eq!(
        prefixes(&hints, "lutris", true),
        [root.join("lutris-flatpak-pfx")],
        "{context}"
    );
    let exes: Vec<(&str, bool, &PathBuf)> = hints
        .iter()
        .filter_map(|h| Some((h.launcher.as_str(), h.flatpak, h.exe.as_ref()?)))
        .collect();
    assert_eq!(
        exes,
        [
            (
                "lutris",
                true,
                &root.join("lutris-flatpak-games/cata/Wow.exe")
            ),
            ("heroic", false, &root.join("heroic-sideload/wotlk/Wow.exe")),
        ],
        "{context}"
    );

    let d = discover(&input(&env), &RulesTable::builtin());
    let context = serde_json::to_string_pretty(&d).unwrap();
    let mut expected = vec![
        row(
            "home/Games/Heroic/Prefixes/World of Warcraft/drive_c/Program Files (x86)/World of Warcraft/_retail_",
            "retail",
            "heroic",
        ),
        row("heroic-proton/pfx/drive_c/Games/Cata", "cata", "heroic"),
        row("heroic-sideload/wotlk", "wotlk", "heroic"),
        row(
            "heroic-flatpak-prefixes/Era/drive_c/Program Files (x86)/World of Warcraft/_classic_era_",
            "era",
            "heroic",
        ),
        row("lutris-flatpak-pfx/drive_c/Games/MoP", "mists", "lutris"),
        row("lutris-flatpak-games/cata", "cata", "lutris"),
    ];
    expected.sort();
    assert_eq!(rows(&d, root), expected, "{context}");
}

/// The Steam homes beyond the native and Flathub roots (Snap, the Flatpak's lowercase
/// `data/steam`), and a WoW client added as a non-Steam shortcut, which only
/// `shortcuts.vdf` names.
#[test]
fn steam_snap_roots_and_non_steam_shortcuts_lead_to_their_clients() {
    let env = TestEnv::new();
    let (root, home) = (env.root(), env.home());
    let snap = home.join("snap/steam/common/.local/share/Steam");
    let snap_pfx = steam_prefix(&snap, 3);
    CustomRoot::at(snap_pfx.join("drive_c/Games/Cata"))
        .version(4, 3, 4, 15595)
        .build();
    let lower = home.join(".var/app/com.valvesoftware.Steam/data/steam");
    let lower_pfx = steam_prefix(&lower, 4);
    // A client outside every prefix, and a shortcut to something else.
    CustomRoot::at(root.join("steam-games/wotlk")).build();
    steam_shortcuts(
        &env,
        Install::Native,
        12_345_678,
        &[
            (
                0x8000_0005,
                &root.join("steam-games/wotlk/Wow.exe"),
                &root.join("steam-games/wotlk"),
            ),
            (
                0x8000_0006,
                Path::new("/usr/bin/supertuxkart"),
                Path::new("/usr/bin"),
            ),
        ],
    );

    let (hints, diagnostics) = collect_hints(&dirs(&env));
    let context = format!("{hints:#?}\n{diagnostics:#?}");
    assert!(diagnostics.is_empty(), "{context}");
    assert_eq!(prefixes(&hints, "steam", false), [snap_pfx], "{context}");
    assert_eq!(prefixes(&hints, "steam", true), [lower_pfx], "{context}");
    let exes: Vec<(&str, bool, &PathBuf)> = hints
        .iter()
        .filter_map(|h| Some((h.launcher.as_str(), h.flatpak, h.exe.as_ref()?)))
        .collect();
    assert_eq!(
        exes,
        [("steam", false, &root.join("steam-games/wotlk/Wow.exe"))],
        "{context}"
    );

    let d = discover(&input(&env), &RulesTable::builtin());
    let context = serde_json::to_string_pretty(&d).unwrap();
    let mut expected = vec![
        row(
            "home/snap/steam/common/.local/share/Steam/steamapps/compatdata/3/pfx/drive_c/Games/Cata",
            "cata",
            "steam",
        ),
        row("steam-games/wotlk", "wotlk", "steam"),
    ];
    expected.sort();
    assert_eq!(rows(&d, root), expected, "{context}");
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
