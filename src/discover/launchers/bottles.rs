//! Bottles, verified against bottlesdevs/Bottles 67.4 (`bottles/backend`). Its base is
//! `$XDG_DATA_HOME/bottles` (`globals.py`), for the Flatpak `data/bottles` in its private
//! home. Bottles live in `<base>/bottles`, or in the directory that `<base>/data.yml`'s
//! `custom_bottles_path` names when the user moved them (`managers/data.py`,
//! `managers/manager.py`, which refuses a document-portal path). Both directories are
//! read: the default one may still hold bottles from before the move. In them, a
//! directory holding `drive_c/` is a prefix. A bottle created at a custom path leaves
//! only a `placeholder.yml` there, whose top-level `Path:` names the bottle; an absolute
//! top-level `Path:` in a `bottle.yml` names it too.

use std::path::{Component, Path, PathBuf};

use super::super::{scan::is_prefix, sorted_dirs};
use super::{Hints, LauncherDirs, expand_home, read_config, yaml_value};

const LAUNCHER: &str = "bottles";
const FLATPAK: &str = "com.usebottles.bottles";

pub(super) fn read(dirs: &LauncherDirs, out: &mut Hints) {
    let bases = [
        (false, dirs.data.join("bottles")),
        (true, dirs.flatpak(FLATPAK).join("data/bottles")),
    ];
    for (flatpak, base) in bases {
        let mut bottle_dirs = vec![base.join("bottles")];
        if let Some(moved) = moved_bottles_dir(&base, &dirs.home, out)
            && !bottle_dirs.contains(&moved)
        {
            bottle_dirs.push(moved);
        }
        for bottle in bottle_dirs.iter().flat_map(|dir| sorted_dirs(dir)) {
            if is_prefix(&bottle) {
                out.prefix(LAUNCHER, flatpak, bottle.clone());
            }
            for file in ["placeholder.yml", "bottle.yml"] {
                let yml = bottle.join(file);
                if !yml.exists() {
                    continue;
                }
                // A path that names nothing (a bottle since deleted) is skipped silently.
                if let Some(text) = read_config(LAUNCHER, &yml, out)
                    && let Some(elsewhere) = top_level_path(&text, "Path", &dirs.home)
                    && elsewhere != bottle
                    && elsewhere.is_dir()
                {
                    out.prefix(LAUNCHER, flatpak, elsewhere);
                }
            }
        }
    }
}

/// The bottles directory chosen in Bottles' preferences: `data.yml`'s
/// `custom_bottles_path` when it is a directory. Bottles refuses a document-portal path
/// (`/run/user/<uid>/doc/…`), and so does this, with a diagnostic.
fn moved_bottles_dir(base: &Path, home: &Path, out: &mut Hints) -> Option<PathBuf> {
    let yml = base.join("data.yml");
    if !yml.exists() {
        return None;
    }
    let text = read_config(LAUNCHER, &yml, out)?;
    let dir = top_level_path(&text, "custom_bottles_path", home)?;
    if is_document_portal(&dir) {
        out.note(format!(
            "{LAUNCHER}: custom_bottles_path {} is a document-portal path, which Bottles does not use",
            dir.display()
        ));
        return None;
    }
    dir.is_dir().then_some(dir)
}

/// Whether `path` lies in the document portal, `/run/user/<uid>/doc/`.
fn is_document_portal(path: &Path) -> bool {
    let parts: Vec<Component> = path.components().take(5).collect();
    matches!(
        parts.as_slice(),
        [Component::RootDir, Component::Normal(run), Component::Normal(user), Component::Normal(_), Component::Normal(doc)]
            if *run == "run" && *user == "user" && *doc == "doc"
    )
}

/// A top-level (unindented) `key:` as an absolute path, after `~` expansion; `None` when
/// it is missing, empty or relative (a bottle's own `Path:` is its directory name).
pub fn top_level_path(text: &str, key: &str, home: &Path) -> Option<PathBuf> {
    text.lines()
        .filter(|line| !line.starts_with([' ', '\t']))
        .find_map(|line| yaml_value(line, &[key]))
        .map(|(_, value)| expand_home(&value, home))
        .filter(|path| path.is_absolute())
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    #[test]
    fn only_an_absolute_top_level_value_is_a_path() {
        let home = Path::new("/h");
        for (text, key, expected) in [
            (
                "Arch: win64\nName: wow\nPath: wow\nRunner: soda-9.0-1\n",
                "Path",
                None,
            ),
            (
                "Name: wow\nCustom_Path: true\nPath: ~/Games/bottles/wow\r\n",
                "Path",
                Some("/h/Games/bottles/wow"),
            ),
            ("Path: /mnt/games/wow2\n", "Path", Some("/mnt/games/wow2")),
            (
                "External_Programs:\n  x:\n    Path: /g/other.exe\nPath: wow\n",
                "Path",
                None,
            ),
            ("", "Path", None),
            (
                "custom_bottles_path: /mnt/b\nnotes: x\n",
                "custom_bottles_path",
                Some("/mnt/b"),
            ),
            (
                "custom_bottles_path: '~/Bottles'\r\n",
                "custom_bottles_path",
                Some("/h/Bottles"),
            ),
            (
                "custom_bottles_path: Bottles\n",
                "custom_bottles_path",
                None,
            ),
            ("Path: /mnt/b\n", "custom_bottles_path", None),
        ] {
            assert_eq!(
                top_level_path(text, key, home),
                expected.map(PathBuf::from),
                "{text:?}"
            );
        }
    }

    #[test]
    fn document_portal_paths() {
        for (path, portal) in [
            ("/run/user/1000/doc/abc123/Bottles", true),
            ("/run/user/1000/doc", true),
            ("/run/user/1000/Bottles", false),
            ("/home/me/run/user/1/doc/x", false),
            ("/mnt/doc", false),
        ] {
            assert_eq!(is_document_portal(Path::new(path)), portal, "{path}");
        }
    }

    proptest! {
        #[test]
        fn any_text_is_handled(text in any::<Vec<u8>>()) {
            let text = String::from_utf8_lossy(&text);
            for key in ["Path", "custom_bottles_path"] {
                let _ = top_level_path(&text, key, Path::new("/h"));
            }
        }
    }
}
