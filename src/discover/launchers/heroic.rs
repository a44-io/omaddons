//! Heroic: the prefixes its configs name and the WoW exes it was given, native or
//! Flatpak, as Heroic v2.22.4 writes them:
//! - `<config>/heroic` is Electron's `appData/heroic` and `userData` alike (package.json
//!   names the app `heroic`; `src/backend/constants/paths.ts:8-39`); the Flatpak
//!   `com.heroicgameslauncher.hgl` keeps it under `~/.var/app/<id>/config/heroic`.
//! - `GamesConfig/<app>.json` is `{"<app>": {"winePrefix": …, "wineVersion": {…}},
//!   "version": "v0", "explicit": true}` (`src/backend/game_config.ts:336-370`): every
//!   string under a `winePrefix` key, at any depth, is a prefix. A Proton game's Wine
//!   prefix is `<winePrefix>/pfx` (`src/backend/launcher.ts:1136-1139`), taken when it
//!   is the one holding `drive_c`.
//! - `config.json` is `{"defaultSettings": {…}}` (`src/backend/config.ts:404-409`): its
//!   `winePrefix` is the shared prefix, and `defaultWinePrefixDir` (`defaultWinePrefix`
//!   in older files; both default to `~/Games/Heroic/Prefixes`, `:305-307`, `:344-345`)
//!   holds one prefix per game, named by its title (`WineSelector/index.tsx:62`): its
//!   direct children that are prefixes are read, when Heroic is installed.
//! - `sideload_apps/library.json` (electron-store `{cwd: 'sideload_apps', name:
//!   'library'}` under `userData`, `src/backend/storeManagers/sideload/electronStores.ts:3-7`)
//!   is `{"games": [{"install": {"executable": …}}]}` (`sideload/library.ts:19-49`): how
//!   Battle.net or a private-server client is added. Its WoW exes are kept.

use std::path::{Path, PathBuf};

use serde_json::Value;

use super::super::sorted_dirs;
use super::{Hints, LauncherDirs, expand_home, files_with_ext, is_client_exe, read_config};

const LAUNCHER: &str = "heroic";
const FLATPAK: &str = "com.heroicgameslauncher.hgl";

pub(super) fn read(dirs: &LauncherDirs, out: &mut Hints) {
    let bases = [
        (false, dirs.config.join("heroic")),
        (true, dirs.flatpak(FLATPAK).join("config/heroic")),
    ];
    for (flatpak, base) in bases {
        if !base.is_dir() {
            continue;
        }
        let prefix = |out: &mut Hints, path: &str| {
            out.prefix(
                LAUNCHER,
                flatpak,
                wine_prefix(expand_home(path, &dirs.home)),
            );
        };
        for file in files_with_ext(&base.join("GamesConfig"), "json") {
            for path in json_file(&file, out, wine_prefixes).unwrap_or_default() {
                prefix(out, &path);
            }
        }
        let config = base.join("config.json");
        let settings = if config.is_file() {
            json_file(&config, out, settings).unwrap_or_default()
        } else {
            Settings::default()
        };
        for path in &settings.prefixes {
            prefix(out, path);
        }
        let folders = if settings.folders.is_empty() {
            vec![dirs.home.join("Games/Heroic/Prefixes")]
        } else {
            let home = &dirs.home;
            settings
                .folders
                .iter()
                .map(|f| expand_home(f, home))
                .collect()
        };
        for folder in folders {
            for child in sorted_dirs(&folder) {
                let child = wine_prefix(child);
                if child.join("drive_c").is_dir() {
                    out.prefix(LAUNCHER, flatpak, child);
                }
            }
        }
        let library = base.join("sideload_apps/library.json");
        if library.is_file() {
            for exe in json_file(&library, out, sideloaded).unwrap_or_default() {
                let exe = expand_home(&exe, &dirs.home);
                if is_client_exe(&exe) && exe.is_absolute() {
                    out.exe(LAUNCHER, flatpak, exe);
                }
            }
        }
    }
}

/// `prefix`, or the `pfx` inside it when that is the one holding `drive_c` (Proton).
fn wine_prefix(prefix: PathBuf) -> PathBuf {
    let pfx = prefix.join("pfx");
    if !prefix.join("drive_c").is_dir() && pfx.join("drive_c").is_dir() {
        pfx
    } else {
        prefix
    }
}

/// `parse` applied to `file`'s text; a diagnostic when it cannot be read or parsed.
fn json_file<T>(
    file: &Path,
    out: &mut Hints,
    parse: impl Fn(&str) -> Result<T, String>,
) -> Option<T> {
    let text = read_config(LAUNCHER, file, out)?;
    parse(&text)
        .map_err(|err| {
            out.note(format!(
                "{LAUNCHER}: {} is not valid JSON ({err}); skipped",
                file.display()
            ));
        })
        .ok()
}

/// The non-empty `winePrefix` strings in a GamesConfig file, depth first in key order.
pub fn wine_prefixes(text: &str) -> Result<Vec<String>, String> {
    let value: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    let mut found = Vec::new();
    // (value, whether it sits under a `winePrefix` key)
    let mut stack = vec![(&value, false)];
    while let Some((v, prefix)) = stack.pop() {
        match v {
            Value::String(s) if prefix && !s.is_empty() => found.push(s.clone()),
            Value::Object(map) => {
                stack.extend(map.iter().rev().map(|(k, c)| (c, k == "winePrefix")));
            }
            Value::Array(items) => stack.extend(items.iter().rev().map(|c| (c, false))),
            _ => {}
        }
    }
    Ok(found)
}

/// What `config.json` says about prefixes.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Settings {
    /// `defaultSettings.winePrefix`: the shared prefix.
    pub prefixes: Vec<String>,
    /// `defaultSettings.defaultWinePrefixDir`, else `defaultWinePrefix`: the folder of
    /// per-game prefixes.
    pub folders: Vec<String>,
}

/// The prefix settings of Heroic's `config.json`.
pub fn settings(text: &str) -> Result<Settings, String> {
    let value: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    let defaults = &value["defaultSettings"];
    let string = |key: &str| {
        defaults[key]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    };
    Ok(Settings {
        prefixes: string("winePrefix").into_iter().collect(),
        folders: string("defaultWinePrefixDir")
            .or_else(|| string("defaultWinePrefix"))
            .into_iter()
            .collect(),
    })
}

/// The `install.executable` of every sideloaded app in `sideload_apps/library.json`.
pub fn sideloaded(text: &str) -> Result<Vec<String>, String> {
    let value: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    Ok(value["games"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|game| game["install"]["executable"].as_str())
        .filter(|exe| !exe.is_empty())
        .map(str::to_owned)
        .collect())
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    #[test]
    fn nested_wine_prefixes() {
        let text = r#"{
            "wow": {"winePrefix": "/g/wow-pfx", "wineVersion": {"bin": "/usr/bin/wine"}},
            "other": {"winePrefix": "~/Games/Heroic/Prefixes/other", "nested": [{"winePrefix": "/g/deep"}]},
            "empty": {"winePrefix": ""},
            "version": "v0"
        }"#;
        assert_eq!(
            wine_prefixes(text).unwrap(),
            ["/g/deep", "~/Games/Heroic/Prefixes/other", "/g/wow-pfx"]
        );
    }

    #[test]
    fn invalid_json_is_an_error() {
        for text in ["", "{\"wow\": {\"winePrefix\": \"/g", "\u{fffd}\0"] {
            assert!(wine_prefixes(text).is_err(), "{text:?}");
            assert!(settings(text).is_err(), "{text:?}");
            assert!(sideloaded(text).is_err(), "{text:?}");
        }
    }

    #[test]
    fn config_json_names_the_shared_prefix_and_the_prefix_folder() {
        let text = r#"{"defaultSettings": {"winePrefix": "~/Games/Heroic/Prefixes/shared",
            "defaultWinePrefixDir": "/g/Prefixes", "defaultWinePrefix": "/g/old"}, "version": "v0"}"#;
        assert_eq!(
            settings(text).unwrap(),
            Settings {
                prefixes: vec!["~/Games/Heroic/Prefixes/shared".into()],
                folders: vec!["/g/Prefixes".into()],
            }
        );
        // An older file names the folder `defaultWinePrefix`.
        let old = r#"{"defaultSettings": {"defaultWinePrefix": "/g/old"}}"#;
        assert_eq!(settings(old).unwrap().folders, ["/g/old"]);
        assert_eq!(settings("{}").unwrap(), Settings::default());
    }

    #[test]
    fn sideloaded_apps_give_their_executables() {
        let text = r#"{"games": [
            {"runner": "sideload", "app_name": "x1", "title": "WoW", "install": {"executable": "/g/wow/Wow.exe", "platform": "Windows", "is_dlc": false}, "folder_name": "/g/wow"},
            {"app_name": "x2", "install": {"platform": "Browser"}},
            {"app_name": "x3", "install": {"executable": ""}}
        ], "installed": []}"#;
        assert_eq!(sideloaded(text).unwrap(), ["/g/wow/Wow.exe"]);
        assert!(sideloaded("{}").unwrap().is_empty());
    }

    #[test]
    fn a_proton_prefix_is_its_pfx() {
        let dir = tempfile::tempdir().unwrap();
        let (wine, proton) = (dir.path().join("wine"), dir.path().join("proton"));
        std::fs::create_dir_all(wine.join("drive_c")).unwrap();
        std::fs::create_dir_all(proton.join("pfx/drive_c")).unwrap();
        assert_eq!(wine_prefix(wine.clone()), wine);
        assert_eq!(wine_prefix(proton.clone()), proton.join("pfx"));
        let missing = dir.path().join("missing");
        assert_eq!(wine_prefix(missing.clone()), missing);
    }

    proptest! {
        #[test]
        fn any_text_is_handled(text in any::<Vec<u8>>()) {
            let text = String::from_utf8_lossy(&text);
            let _ = wine_prefixes(&text);
            let _ = settings(&text);
            let _ = sideloaded(&text);
        }
    }
}
