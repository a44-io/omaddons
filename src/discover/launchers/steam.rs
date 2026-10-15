//! Steam: every library `steamapps/libraryfolders.vdf` lists, plus the Steam root itself,
//! contributes its `steamapps/compatdata/*/pfx` prefixes (WoW is usually a non-Steam
//! shortcut with its own compatdata id). Native roots are `<data>/Steam` and the
//! `~/.steam` links (`steam`, `root`, and Debian's `debian-installation`); the Flatpak
//! keeps its root under its private home. Roots and libraries are read once each by
//! canonical path, since the `~/.steam` links point at `<data>/Steam`.
//!
//! Verified 2026-10-15 on a native install: `<data>/Steam/steamapps/libraryfolders.vdf`
//! (an identical copy sits in `config/`), and `~/.steam/steam` and `~/.steam/root` link
//! to `<data>/Steam`. The Flatpak locations are unverified.

use std::path::{Path, PathBuf};

use super::super::sorted_dirs;
use super::{Hints, LauncherDirs, read_config};

const LAUNCHER: &str = "steam";
const FLATPAK: &str = "com.valvesoftware.Steam";

pub(super) fn read(dirs: &LauncherDirs, out: &mut Hints) {
    let flatpak = dirs.flatpak(FLATPAK);
    let roots = [
        (false, dirs.data.join("Steam")),
        (false, dirs.home.join(".local/share/Steam")),
        (false, dirs.home.join(".steam/steam")),
        (false, dirs.home.join(".steam/root")),
        (false, dirs.home.join(".steam/debian-installation")),
        (true, flatpak.join(".local/share/Steam")),
        (true, flatpak.join("data/Steam")),
    ];
    let (mut seen_roots, mut seen_libraries) = (Vec::new(), Vec::new());
    for (is_flatpak, root) in roots {
        if !once(&root, &mut seen_roots) {
            continue;
        }
        let mut libraries = vec![root.clone()];
        let vdf = ["steamapps/libraryfolders.vdf", "config/libraryfolders.vdf"]
            .into_iter()
            .map(|rel| root.join(rel))
            .find(|p| p.exists());
        if let Some(vdf) = vdf
            && let Some(text) = read_config(LAUNCHER, &vdf, out)
        {
            libraries.extend(library_paths(&text).into_iter().map(PathBuf::from));
        }
        for library in libraries {
            if !once(&library, &mut seen_libraries) {
                continue;
            }
            for app in sorted_dirs(&library.join("steamapps/compatdata")) {
                let pfx = app.join("pfx");
                if pfx.is_dir() {
                    out.prefix(LAUNCHER, is_flatpak, pfx);
                }
            }
        }
    }
}

/// True the first time an existing directory is seen, by canonical path.
fn once(dir: &Path, seen: &mut Vec<PathBuf>) -> bool {
    let Ok(canonical) = std::fs::canonicalize(dir) else {
        return false;
    };
    if !canonical.is_dir() || seen.contains(&canonical) {
        return false;
    }
    seen.push(canonical);
    true
}

/// The `"path"` values of a `libraryfolders.vdf`, in file order, with `\\` and `\"`
/// unescaped.
pub fn library_paths(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            let (key, rest) = (line.get(..6)?, line.get(6..)?);
            if !key.eq_ignore_ascii_case("\"path\"") || !rest.starts_with([' ', '\t']) {
                return None;
            }
            let quoted = rest.trim_start().strip_prefix('"')?.strip_suffix('"')?;
            let mut path = String::new();
            let mut chars = quoted.chars();
            while let Some(c) = chars.next() {
                match c {
                    '\\' => path.extend(chars.next()),
                    c => path.push(c),
                }
            }
            (!path.is_empty()).then_some(path)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    #[test]
    fn library_paths_from_the_vdf_steam_writes() {
        let text = "\"libraryfolders\"\r\n{\r\n\t\"0\"\r\n\t{\r\n\t\t\"path\"\t\t\"/h/.local/share/Steam\"\r\n\t\t\"label\"\t\t\"\"\r\n\t\t\"apps\"\r\n\t\t{\r\n\t\t\t\"228980\"\t\t\"102931551\"\r\n\t\t}\r\n\t}\r\n\t\"1\"\r\n\t{\r\n\t\t\"path\"\t\t\"/mnt/games/Steam\\\\Library \\\"2\\\"\"\r\n\t}\r\n}\r\n";
        assert_eq!(
            library_paths(text),
            ["/h/.local/share/Steam", "/mnt/games/Steam\\Library \"2\""]
        );
    }

    #[test]
    fn other_lines_and_broken_values_are_ignored() {
        let text =
            "\"pathx\"\t\"/a\"\n\"path\"\"/b\"\n\"path\"\t\"/c\n\"path\"\t\"\"\n\"PATH\" \"/d\"\n";
        assert_eq!(library_paths(text), ["/d"]);
        assert!(library_paths("").is_empty());
        assert!(library_paths("\u{fffd}\0\"path\"").is_empty());
    }

    proptest! {
        #[test]
        fn any_text_is_handled(text in any::<Vec<u8>>()) {
            let _ = library_paths(&String::from_utf8_lossy(&text));
        }
    }
}
