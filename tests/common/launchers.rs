//! Launcher configuration fixtures at the locations the launcher readers search, laid
//! out as the upstream sources each reader cites (keep the two in sync, and build a new
//! fixture from those sources, never from the reader): Lutris, Heroic, Steam and
//! Bottles, native, Flatpak and Snap.

use std::fs;
use std::path::{Path, PathBuf};

use super::env::TestEnv;

/// A native or a Flatpak install of a launcher.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Install {
    Native,
    Flatpak,
}

/// Lutris keeps game files in either directory, native or inside its Flatpak.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LutrisDir {
    /// `<config>/lutris/games`.
    Config,
    /// `<data>/lutris/games`.
    Data,
    /// `~/.var/app/net.lutris.Lutris/config/lutris/games`.
    FlatpakConfig,
    /// `~/.var/app/net.lutris.Lutris/data/lutris/games`.
    FlatpakData,
}

/// How [`malform`] damages a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Malformed {
    /// The first half of the current contents.
    Truncated,
    /// Bytes that are not UTF-8.
    Binary,
    Empty,
}

fn write(path: &Path, text: &str) -> PathBuf {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
    path.to_path_buf()
}

/// `lutris/games/<name>.yml` with `game.exe` and `game.prefix` when given.
pub fn lutris_game(
    env: &TestEnv,
    dir: LutrisDir,
    name: &str,
    exe: Option<&Path>,
    prefix: Option<&Path>,
) -> PathBuf {
    let flatpak = env.home().join(".var/app/net.lutris.Lutris");
    let base = match dir {
        LutrisDir::Config => env.config(),
        LutrisDir::Data => env.data(),
        LutrisDir::FlatpakConfig => flatpak.join("config"),
        LutrisDir::FlatpakData => flatpak.join("data"),
    };
    let mut yaml = "game:\n".to_owned();
    if let Some(exe) = exe {
        yaml.push_str(&format!("  exe: {}\n", exe.display()));
    }
    if let Some(prefix) = prefix {
        yaml.push_str(&format!(
            "  prefix: '{}'  # the Wine prefix\n",
            prefix.display()
        ));
    }
    yaml.push_str(&format!("name: {name}\nrunner: wine\nslug: {name}\n"));
    write(
        &base.join("lutris/games").join(format!("{name}.yml")),
        &yaml,
    )
}

/// Heroic's config directory: `<config>/heroic` or the Flatpak's.
pub fn heroic_dir(env: &TestEnv, install: Install) -> PathBuf {
    match install {
        Install::Native => env.config().join("heroic"),
        Install::Flatpak => env
            .home()
            .join(".var/app/com.heroicgameslauncher.hgl/config/heroic"),
    }
}

/// `GamesConfig/<app>.json` with a `winePrefix`, as Heroic v2.22.4 writes it
/// (`src/backend/game_config.ts:336-370`).
pub fn heroic_game(env: &TestEnv, install: Install, app: &str, wine_prefix: &Path) -> PathBuf {
    let json = serde_json::json!({
        app: {
            "winePrefix": wine_prefix,
            "wineVersion": { "bin": "/usr/bin/wine", "name": "Wine - System", "type": "wine" },
            "language": "en"
        },
        "version": "v0",
        "explicit": true
    });
    write(
        &heroic_dir(env, install)
            .join("GamesConfig")
            .join(format!("{app}.json")),
        &serde_json::to_string_pretty(&json).unwrap(),
    )
}

/// `config.json` as Heroic v2.22.4 writes it (`src/backend/config.ts:404-409`), with
/// `defaultSettings.winePrefix` (the shared prefix) and `defaultWinePrefixDir` (the
/// folder of per-game prefixes) when given.
pub fn heroic_settings(
    env: &TestEnv,
    install: Install,
    shared: Option<&Path>,
    folder: Option<&Path>,
) -> PathBuf {
    let mut settings = serde_json::json!({ "language": "en", "maxWorkers": 0 });
    if let Some(shared) = shared {
        settings["winePrefix"] = serde_json::json!(shared);
    }
    if let Some(folder) = folder {
        settings["defaultWinePrefixDir"] = serde_json::json!(folder);
        settings["defaultWinePrefix"] = serde_json::json!(folder);
    }
    let json = serde_json::json!({ "defaultSettings": settings, "version": "v0" });
    write(
        &heroic_dir(env, install).join("config.json"),
        &serde_json::to_string_pretty(&json).unwrap(),
    )
}

/// `sideload_apps/library.json` as Heroic v2.22.4 writes it
/// (`src/backend/storeManagers/sideload/library.ts:19-49`): one sideloaded app per exe.
pub fn heroic_sideloaded(env: &TestEnv, install: Install, exes: &[&Path]) -> PathBuf {
    let games: Vec<serde_json::Value> = exes
        .iter()
        .enumerate()
        .map(|(i, exe)| {
            serde_json::json!({
                "runner": "sideload",
                "app_name": format!("sideload{i}"),
                "title": "World of Warcraft",
                "install": { "executable": exe, "platform": "Windows", "is_dlc": false },
                "folder_name": exe.parent(),
                "is_installed": true,
                "canRunOffline": true
            })
        })
        .collect();
    let json = serde_json::json!({ "games": games, "installed": [] });
    write(
        &heroic_dir(env, install).join("sideload_apps/library.json"),
        &serde_json::to_string_pretty(&json).unwrap(),
    )
}

/// The Steam root: `<data>/Steam` or the Flatpak's.
pub fn steam_root(env: &TestEnv, install: Install) -> PathBuf {
    match install {
        Install::Native => env.data().join("Steam"),
        Install::Flatpak => env
            .home()
            .join(".var/app/com.valvesoftware.Steam/.local/share/Steam"),
    }
}

/// `steamapps/libraryfolders.vdf` in the Steam root listing `libraries` (the root
/// itself first, as Steam writes it).
pub fn steam_libraries(env: &TestEnv, install: Install, libraries: &[&Path]) -> PathBuf {
    let root = steam_root(env, install);
    let mut vdf = "\"libraryfolders\"\n{\n".to_owned();
    let all = std::iter::once(root.as_path()).chain(libraries.iter().copied());
    for (i, path) in all.enumerate() {
        let escaped = path.display().to_string().replace('\\', "\\\\");
        vdf.push_str(&format!(
            "\t\"{i}\"\n\t{{\n\t\t\"path\"\t\t\"{escaped}\"\n\t\t\"label\"\t\t\"\"\n\t\t\"apps\"\n\t\t{{\n\t\t}}\n\t}}\n"
        ));
    }
    vdf.push_str("}\n");
    write(&root.join("steamapps/libraryfolders.vdf"), &vdf)
}

/// A binary `shortcuts.vdf` as Steam writes it: a `shortcuts` map of numbered maps, one
/// per `(appid, exe, start dir)`, the paths in double quotes, among Steam's other fields
/// and a `tags` map. Each field is a type byte (`0x00` map, `0x01` string, `0x02`
/// int32), a NUL-terminated key and its value; `0x08` closes a map.
pub fn shortcuts_vdf(shortcuts: &[(u32, &Path, &Path)]) -> Vec<u8> {
    let string = |b: &mut Vec<u8>, key: &str, value: &str| {
        b.push(0x01);
        b.extend(key.as_bytes());
        b.push(0);
        b.extend(value.as_bytes());
        b.push(0);
    };
    let int = |b: &mut Vec<u8>, key: &str, value: u32| {
        b.push(0x02);
        b.extend(key.as_bytes());
        b.push(0);
        b.extend(value.to_le_bytes());
    };
    let mut b = b"\x00shortcuts\x00".to_vec();
    for (i, (app_id, exe, dir)) in shortcuts.iter().enumerate() {
        b.extend(format!("\x00{i}\x00").as_bytes());
        int(&mut b, "appid", *app_id);
        string(&mut b, "AppName", "World of Warcraft");
        string(&mut b, "Exe", &format!("\"{}\"", exe.display()));
        string(&mut b, "StartDir", &format!("\"{}\"", dir.display()));
        string(&mut b, "icon", "");
        string(&mut b, "ShortcutPath", "");
        string(&mut b, "LaunchOptions", "");
        int(&mut b, "IsHidden", 0);
        int(&mut b, "AllowDesktopConfig", 1);
        int(&mut b, "AllowOverlay", 1);
        int(&mut b, "OpenVR", 0);
        int(&mut b, "LastPlayTime", 0);
        b.extend(b"\x00tags\x00\x08");
        b.push(0x08);
    }
    b.extend(b"\x08\x08");
    b
}

/// `<steam root>/userdata/<user>/config/shortcuts.vdf` holding `shortcuts`.
pub fn steam_shortcuts(
    env: &TestEnv,
    install: Install,
    user: u32,
    shortcuts: &[(u32, &Path, &Path)],
) -> PathBuf {
    let path = steam_root(env, install).join(format!("userdata/{user}/config/shortcuts.vdf"));
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, shortcuts_vdf(shortcuts)).unwrap();
    path
}

/// `<library>/steamapps/compatdata/<app_id>/pfx` with a `drive_c/`; returns the prefix.
pub fn steam_prefix(library: &Path, app_id: u32) -> PathBuf {
    let pfx = library.join(format!("steamapps/compatdata/{app_id}/pfx"));
    fs::create_dir_all(pfx.join("drive_c")).unwrap();
    pfx
}

/// Bottles' base: `<data>/bottles` or the Flatpak's `data/bottles`.
pub fn bottles_base(env: &TestEnv, install: Install) -> PathBuf {
    match install {
        Install::Native => env.data().join("bottles"),
        Install::Flatpak => env
            .home()
            .join(".var/app/com.usebottles.bottles/data/bottles"),
    }
}

/// The default bottles directory: `<base>/bottles`.
pub fn bottles_dir(env: &TestEnv, install: Install) -> PathBuf {
    bottles_base(env, install).join("bottles")
}

/// A bottle with `drive_c/` and a `bottle.yml` whose `Path:` is its name, in the default
/// bottles directory; returns the bottle directory.
pub fn bottle(env: &TestEnv, install: Install, name: &str) -> PathBuf {
    bottle_in(&bottles_dir(env, install), name)
}

/// A bottle as [`bottle`] makes it, in `dir` (a moved bottles directory).
pub fn bottle_in(dir: &Path, name: &str) -> PathBuf {
    let bottle = dir.join(name);
    fs::create_dir_all(bottle.join("drive_c")).unwrap();
    write(
        &bottle.join("bottle.yml"),
        &format!(
            "Arch: win64\nName: {name}\nPath: {name}\nCustom_Path: false\nRunner: soda-9.0-1\n"
        ),
    );
    bottle
}

/// `<base>/data.yml` naming `dir` as the moved bottles directory (Bottles' preferences);
/// returns the file.
pub fn bottles_data(env: &TestEnv, install: Install, dir: &Path) -> PathBuf {
    let yml = bottles_base(env, install).join("data.yml");
    write(&yml, &format!("custom_bottles_path: {}\n", dir.display()));
    yml
}

/// A bottle created at the custom path `dir`, as Bottles 67.4 writes it: the bottle
/// (`drive_c/`, a `bottle.yml` with `Custom_Path: true` and its absolute `Path`) at
/// `dir`, and only a `placeholder.yml` naming it in the default bottles directory.
/// Returns the placeholder.
pub fn custom_path_bottle(env: &TestEnv, install: Install, name: &str, dir: &Path) -> PathBuf {
    fs::create_dir_all(dir.join("drive_c")).unwrap();
    let path = dir.display();
    write(
        &dir.join("bottle.yml"),
        &format!(
            "Arch: win64\nName: {name}\nPath: {path}\nCustom_Path: true\nRunner: soda-9.0-1\n"
        ),
    );
    let placeholder = bottles_dir(env, install).join(name).join("placeholder.yml");
    write(&placeholder, &format!("Path: {path}\n"));
    placeholder
}

/// Damages a launcher file in place.
pub fn malform(path: &Path, how: Malformed) {
    let bytes = match how {
        Malformed::Truncated => {
            let mut bytes = fs::read(path).unwrap_or_default();
            bytes.truncate(bytes.len() / 2);
            bytes
        }
        Malformed::Binary => vec![0xff, 0xfe, 0x00, 0x9f, 0x80, b'{', 0xc3],
        Malformed::Empty => Vec::new(),
    };
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}
