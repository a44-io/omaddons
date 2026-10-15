//! Launcher configuration fixtures at the locations the launcher readers search
//! (discover-launchers bead; several of those paths are unverified upstream, so keep the
//! two in sync): Lutris, Heroic, Steam and Bottles, native and Flatpak.

use std::fs;
use std::path::{Path, PathBuf};

use super::env::TestEnv;

/// A native or a Flatpak install of a launcher.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Install {
    Native,
    Flatpak,
}

/// Lutris keeps game files in either directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LutrisDir {
    /// `<config>/lutris/games`.
    Config,
    /// `<data>/lutris/games`.
    Data,
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
    let base = match dir {
        LutrisDir::Config => env.config(),
        LutrisDir::Data => env.data(),
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

/// `GamesConfig/<app>.json` with a `winePrefix`.
pub fn heroic_game(env: &TestEnv, install: Install, app: &str, wine_prefix: &Path) -> PathBuf {
    let json = serde_json::json!({
        app: {
            "winePrefix": wine_prefix,
            "wineVersion": { "bin": "/usr/bin/wine", "name": "Wine - System", "type": "wine" },
            "language": "en"
        },
        "version": "v0"
    });
    write(
        &heroic_dir(env, install)
            .join("GamesConfig")
            .join(format!("{app}.json")),
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

/// `<library>/steamapps/compatdata/<app_id>/pfx` with a `drive_c/`; returns the prefix.
pub fn steam_prefix(library: &Path, app_id: u32) -> PathBuf {
    let pfx = library.join(format!("steamapps/compatdata/{app_id}/pfx"));
    fs::create_dir_all(pfx.join("drive_c")).unwrap();
    pfx
}

/// The bottles directory: `<data>/bottles/bottles` or the Flatpak's.
pub fn bottles_dir(env: &TestEnv, install: Install) -> PathBuf {
    match install {
        Install::Native => env.data().join("bottles/bottles"),
        Install::Flatpak => env
            .home()
            .join(".var/app/com.usebottles.bottles/data/bottles/bottles"),
    }
}

/// A bottle with `drive_c/` and a `bottle.yml` whose `Path:` is the override when given
/// (else the bottle's name); returns the bottle directory.
pub fn bottle(
    env: &TestEnv,
    install: Install,
    name: &str,
    path_override: Option<&Path>,
) -> PathBuf {
    let dir = bottles_dir(env, install).join(name);
    fs::create_dir_all(dir.join("drive_c")).unwrap();
    let path = path_override.map_or_else(|| name.to_owned(), |p| p.display().to_string());
    write(
        &dir.join("bottle.yml"),
        &format!("Arch: win64\nName: {name}\nPath: {path}\nRunner: soda-9.0-1\n"),
    );
    dir
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
