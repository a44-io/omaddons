//! Bottles: one directory per bottle under `<data>/bottles/bottles`, or the Flatpak's
//! `data/bottles/bottles`. A bottle directory holding `drive_c/` is a prefix, and an
//! absolute top-level `Path:` in its `bottle.yml` names a bottle kept elsewhere (a
//! custom path). The keys and the Flatpak location are unverified.

use std::path::{Path, PathBuf};

use super::super::{scan::is_prefix, sorted_dirs};
use super::{Hints, LauncherDirs, expand_home, read_config, yaml_value};

const LAUNCHER: &str = "bottles";
const FLATPAK: &str = "com.usebottles.bottles";

pub(super) fn read(dirs: &LauncherDirs, out: &mut Hints) {
    let bases = [
        (false, dirs.data.join("bottles/bottles")),
        (true, dirs.flatpak(FLATPAK).join("data/bottles/bottles")),
    ];
    for (flatpak, base) in bases {
        for bottle in sorted_dirs(&base) {
            if is_prefix(&bottle) {
                out.prefix(LAUNCHER, flatpak, bottle.clone());
            }
            let yml = bottle.join("bottle.yml");
            if !yml.exists() {
                continue;
            }
            if let Some(text) = read_config(LAUNCHER, &yml, out)
                && let Some(custom) = custom_path(&text, &dirs.home)
                && custom != bottle
            {
                out.prefix(LAUNCHER, flatpak, custom);
            }
        }
    }
}

/// An absolute top-level `Path:` (after `~` expansion); a relative one is the bottle's
/// own directory name.
pub fn custom_path(text: &str, home: &Path) -> Option<PathBuf> {
    text.lines()
        .filter(|line| !line.starts_with([' ', '\t']))
        .find_map(|line| yaml_value(line, &["Path"]))
        .map(|(_, value)| expand_home(&value, home))
        .filter(|path| path.is_absolute())
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    #[test]
    fn only_an_absolute_top_level_path_is_custom() {
        let home = Path::new("/h");
        let yml = "Arch: win64\nName: wow\nPath: wow\nRunner: soda-9.0-1\n";
        assert_eq!(custom_path(yml, home), None);
        let yml = "Name: wow\nCustom_Path: true\nPath: ~/Games/bottles/wow\r\n";
        assert_eq!(
            custom_path(yml, home),
            Some(PathBuf::from("/h/Games/bottles/wow"))
        );
        let yml = "External_Programs:\n  x:\n    Path: /g/other.exe\nPath: wow\n";
        assert_eq!(custom_path(yml, home), None);
        assert_eq!(custom_path("", home), None);
    }

    proptest! {
        #[test]
        fn any_text_is_handled(text in any::<Vec<u8>>()) {
            let _ = custom_path(&String::from_utf8_lossy(&text), Path::new("/h"));
        }
    }
}
