//! Lutris: `games/*.yml` under `<config>/lutris` and `<data>/lutris`, native and in the
//! Flatpak `net.lutris.Lutris` (`~/.var/app/<id>/{config,data}`), as Lutris v0.5.22
//! keeps them:
//! - `CONFIG_DIR` is `<config>/lutris`, or `DATA_DIR` (`<data>/lutris`) when that does
//!   not exist; game files are `CONFIG_DIR/games/<slug>-<timestamp>.yml`
//!   (`lutris/settings.py:21-33`, `lutris/config.py:15-26`), so both are read. Inside the
//!   Flatpak, GLib's directories are the app's `config` and `data` (Flatpak docs
//!   `conventions.rst:365-378`; the app id from `lutris/util/xdgshortcuts.py:18-21`).
//! - The wine runner's `game:` options are `exe`, `prefix` and `working_dir`
//!   (`lutris/runners/wine.py:262-316`); neither `wine:` nor `system:` has a prefix.
//!   Every prefix is kept, since scanning a prefix that holds another game is bounded
//!   and harmless; an exe is kept only when it is a WoW client or launcher.
//! - Lutris resolves a relative `exe` against the game's `directory` in `pga.db`
//!   (`wine.py:703-717`, `lutris/runners/runner.py:54-63`). Without a SQLite reader,
//!   omaddons tries `working_dir`, then `prefix`, and says so when neither is set.

use std::path::{Path, PathBuf};

use super::{
    Hints, LauncherDirs, expand_home, files_with_ext, is_client_exe, read_config, yaml_value,
};

const LAUNCHER: &str = "lutris";
const FLATPAK: &str = "net.lutris.Lutris";

pub(super) fn read(dirs: &LauncherDirs, out: &mut Hints) {
    let flatpak = dirs.flatpak(FLATPAK);
    let bases = [
        (false, dirs.config.clone()),
        (false, dirs.data.clone()),
        (true, flatpak.join("config")),
        (true, flatpak.join("data")),
    ];
    for (is_flatpak, base) in bases {
        for file in files_with_ext(&base.join("lutris/games"), "yml") {
            let Some(text) = read_config(LAUNCHER, &file, out) else {
                continue;
            };
            let game = parse(&text, &dirs.home);
            if let Some(prefix) = &game.prefix {
                out.prefix(LAUNCHER, is_flatpak, prefix.clone());
            }
            match game.client_exe() {
                Ok(Some(exe)) => out.exe(LAUNCHER, is_flatpak, exe),
                Ok(None) => {}
                Err(exe) => out.note(format!(
                    "{LAUNCHER}: {} names {exe}, a relative path with no directory it resolves against",
                    file.display()
                )),
            }
        }
    }
}

/// What one game file names; the first line of each key wins.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Game {
    pub exe: Option<PathBuf>,
    pub prefix: Option<PathBuf>,
    pub working_dir: Option<PathBuf>,
}

/// The keys of a Lutris game file, from any text.
pub fn parse(text: &str, home: &Path) -> Game {
    let mut game = Game::default();
    for line in text.lines() {
        let Some((key, value)) = yaml_value(line, &["exe", "prefix", "working_dir"]) else {
            continue;
        };
        let slot = match key {
            "exe" => &mut game.exe,
            "prefix" => &mut game.prefix,
            _ => &mut game.working_dir,
        };
        if slot.is_none() {
            *slot = Some(expand_home(&value, home));
        }
    }
    game
}

impl Game {
    /// The exe when it is a WoW client or launcher, made absolute: a relative exe
    /// resolves against `working_dir`, then `prefix`, preferring one where it exists.
    /// `Err` carries a relative exe with neither directory.
    pub fn client_exe(&self) -> Result<Option<PathBuf>, String> {
        let Some(exe) = &self.exe else {
            return Ok(None);
        };
        if !is_client_exe(exe) {
            return Ok(None);
        }
        if exe.is_absolute() {
            return Ok(Some(exe.clone()));
        }
        let bases = [&self.working_dir, &self.prefix];
        let joined: Vec<PathBuf> = bases
            .iter()
            .filter_map(|b| b.as_ref().map(|b| b.join(exe)))
            .collect();
        match joined
            .iter()
            .find(|p| p.is_file())
            .or_else(|| joined.first())
        {
            Some(path) => Ok(Some(path.clone())),
            None => Err(exe.display().to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    const HOME: &str = "/h";

    fn game(text: &str) -> Game {
        parse(text, Path::new(HOME))
    }

    #[test]
    fn quoted_unquoted_commented_and_crlf() {
        let text = "game:\r\n  exe: /g/World of Warcraft/_retail_/Wow.exe\r\n  prefix: '~/Games/wow'  # pfx\r\nname: wow\r\nsystem:\r\n  prefix_command: gamemoderun\r\n";
        assert_eq!(
            game(text),
            Game {
                exe: Some(PathBuf::from("/g/World of Warcraft/_retail_/Wow.exe")),
                prefix: Some(PathBuf::from("/h/Games/wow")),
                working_dir: None,
            }
        );
        let text = "game:\n  exe: \"/g/wow 335/Wow.exe\"\n  prefix: \"/g/pfx\"\n";
        assert_eq!(game(text).exe, Some(PathBuf::from("/g/wow 335/Wow.exe")));
    }

    #[test]
    fn only_client_exes_are_kept() {
        let exe = |e: &str| {
            Game {
                exe: Some(PathBuf::from(e)),
                ..Game::default()
            }
            .client_exe()
        };
        assert_eq!(
            exe("/g/WowClassic.exe"),
            Ok(Some(PathBuf::from("/g/WowClassic.exe")))
        );
        assert_eq!(exe("/g/WOW.EXE"), Ok(Some(PathBuf::from("/g/WOW.EXE"))));
        assert!(exe("/g/World of Warcraft Launcher.exe").unwrap().is_some());
        assert!(exe("/g/Battle.net Launcher.exe").unwrap().is_some());
        assert_eq!(exe("/g/Diablo IV.exe"), Ok(None));
        assert_eq!(exe("/usr/bin/supertuxkart"), Ok(None));
        assert_eq!(exe("Wow.exe"), Err("Wow.exe".into()));
    }

    #[test]
    fn relative_exes_resolve_against_working_dir_then_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let prefix = dir.path().join("pfx");
        std::fs::create_dir_all(prefix.join("drive_c/wow")).unwrap();
        std::fs::write(prefix.join("drive_c/wow/Wow.exe"), "").unwrap();
        let g = Game {
            exe: Some(PathBuf::from("drive_c/wow/Wow.exe")),
            prefix: Some(prefix.clone()),
            working_dir: Some(dir.path().join("elsewhere")),
        };
        assert_eq!(g.client_exe(), Ok(Some(prefix.join("drive_c/wow/Wow.exe"))));
        let missing = Game {
            exe: Some(PathBuf::from("Wow.exe")),
            prefix: None,
            working_dir: Some(PathBuf::from("/g/wow")),
        };
        assert_eq!(
            missing.client_exe(),
            Ok(Some(PathBuf::from("/g/wow/Wow.exe")))
        );
    }

    #[test]
    fn truncated_binary_and_empty_files_name_nothing() {
        assert_eq!(game(""), Game::default());
        assert_eq!(game("game:\n  exe"), Game::default());
        assert_eq!(game("\u{fffd}\u{fffd}{\0"), Game::default());
    }

    proptest! {
        #[test]
        fn any_text_parses(text in any::<Vec<u8>>()) {
            let g = game(&String::from_utf8_lossy(&text));
            let _ = g.client_exe();
        }
    }
}
