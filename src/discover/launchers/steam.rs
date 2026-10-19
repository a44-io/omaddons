//! Steam: every library `libraryfolders.vdf` lists, plus the Steam root itself,
//! contributes its `steamapps/compatdata/*/pfx` prefixes, and every account's
//! `userdata/<id>/config/shortcuts.vdf` its non-Steam shortcuts whose `Exe` is a WoW
//! client (WoW is usually added as one, with its own compatdata id). Roots and libraries
//! are read once each by canonical path, since the `~/.steam` links point at the root.
//!
//! Steam is closed source; the layout follows the established readers:
//! - Roots, as Lutris v0.5.22 searches them (`lutris/util/steam/config.py:14-25`):
//!   `~/.steam/debian-installation`, `~/.steam`, `~/.local/share/{steam,Steam}`,
//!   `~/snap/steam/common/.local/share/Steam`, `~/.steam/steam`, and the Flatpak's
//!   `~/.var/app/com.valvesoftware.Steam/data/{steam,Steam}`; protontricks 1.15.0
//!   (`src/protontricks/steam.py:29-44`, `:477-499`) adds `~/.steam/root`, the Flatpak's
//!   `.local/share/Steam` (where the Flathub wrapper now moves it,
//!   `flathub/com.valvesoftware.Steam` `steam_wrapper/steam_wrapper.py:330-346`) and
//!   `~/.snap/data/steam/common/.local/share/Steam`. Lutris's system-wide
//!   `/usr/share/steam` and `/usr/local/share/steam` hold Steam's bootstrap, not a
//!   library, and lie outside the home, so they are not read.
//! - `libraryfolders.vdf`: `steamapps/` (protontricks `steam.py:1298-1301`) or `config/`
//!   (Lutris `config.py:196`); each library is a `"path"` value, or in older files a
//!   bare path under a numeric key (protontricks `steam.py:1264-1279`).
//! - `shortcuts.vdf`: the binary VDF of ValvePython `vdf` v3.4 (`vdf/__init__.py:286-420`),
//!   as Lutris writes it (`lutris/util/steam/shortcut.py:150-164`: `appid` signed,
//!   `Exe` and `StartDir` in double quotes) and protontricks reads it (keys in any case,
//!   `startdir` unquoted, `appid & 0xffffffff` naming `compatdata/<appid>`,
//!   `steam.py:1565-1608`). Other writers leave `StartDir` or both unquoted
//!   (`PhilipK/steam_shortcuts_util` test data), so one pair of quotes is stripped when
//!   present.
//!
//! Verified 2026-10-15 on a native install: `<data>/Steam/steamapps/libraryfolders.vdf`
//! (an identical copy sits in `config/`), and `~/.steam/steam` and `~/.steam/root` link
//! to `<data>/Steam`.

use std::path::{Path, PathBuf};

use super::super::{read_bytes, sorted_dirs};
use super::{Hints, LauncherDirs, expand_home, is_client_exe, read_config};

const LAUNCHER: &str = "steam";
const FLATPAK: &str = "com.valvesoftware.Steam";

pub(super) fn read(dirs: &LauncherDirs, out: &mut Hints) {
    let flatpak = dirs.flatpak(FLATPAK);
    let roots = [
        (false, dirs.data.join("Steam")),
        (false, dirs.data.join("steam")),
        (false, dirs.home.join(".local/share/Steam")),
        (false, dirs.home.join(".local/share/steam")),
        (false, dirs.home.join(".steam/steam")),
        (false, dirs.home.join(".steam/root")),
        (false, dirs.home.join(".steam/debian-installation")),
        (false, dirs.home.join(".steam")),
        (
            false,
            dirs.home.join("snap/steam/common/.local/share/Steam"),
        ),
        (
            false,
            dirs.home.join(".snap/data/steam/common/.local/share/Steam"),
        ),
        (true, flatpak.join(".local/share/Steam")),
        (true, flatpak.join("data/Steam")),
        (true, flatpak.join("data/steam")),
    ];
    let (mut seen_roots, mut seen_libraries) = (Vec::new(), Vec::new());
    for (is_flatpak, root) in roots {
        if !once(&root, &mut seen_roots) {
            continue;
        }
        // Each account's non-Steam shortcuts: WoW is usually added as one.
        for user in sorted_dirs(&root.join("userdata")) {
            let file = user.join("config/shortcuts.vdf");
            if !file.is_file() {
                continue;
            }
            let Some(bytes) = read_bytes(&file) else {
                out.note(format!("{LAUNCHER}: cannot read {}", file.display()));
                continue;
            };
            for shortcut in shortcuts(&bytes) {
                if let Some(exe) = shortcut.client_exe(&dirs.home) {
                    out.exe(LAUNCHER, is_flatpak, exe);
                }
            }
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

/// The library paths of a `libraryfolders.vdf`, in file order: each `"path"` value, and
/// in older files each absolute path under a numeric key (`"1" "/mnt/games"`); `\\` and
/// `\"` unescaped. A numeric key under `apps` maps to a size, never a path.
pub fn library_paths(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|line| {
            let (key, rest) = line.trim().strip_prefix('"')?.split_once('"')?;
            if !rest.starts_with([' ', '\t']) {
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
            let numbered = !key.is_empty() && key.bytes().all(|b| b.is_ascii_digit());
            let library = key.eq_ignore_ascii_case("path") || (numbered && path.starts_with('/'));
            (library && !path.is_empty()).then_some(path)
        })
        .collect()
}

/// One non-Steam shortcut of `userdata/<id>/config/shortcuts.vdf`.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Shortcut {
    /// `appid`, as the unsigned number that names its `compatdata` folder.
    pub app_id: Option<u32>,
    /// `Exe` (`exe` in older files), without the quotes Steam writes around it.
    pub exe: Option<String>,
    /// `StartDir`, likewise.
    pub start_dir: Option<String>,
}

impl Shortcut {
    /// The exe when it is a WoW client or launcher, made absolute against `StartDir`
    /// when it is relative; `None` otherwise.
    pub fn client_exe(&self, home: &Path) -> Option<PathBuf> {
        let exe = expand_home(self.exe.as_deref()?, home);
        if !is_client_exe(&exe) {
            return None;
        }
        if exe.is_absolute() {
            return Some(exe);
        }
        Some(expand_home(self.start_dir.as_deref()?, home).join(exe))
    }
}

/// The shortcuts of a binary `shortcuts.vdf`: a `shortcuts` map of numbered maps, one
/// per shortcut. Each entry is a type byte, a NUL-terminated key and its value: `0x00` a
/// map closed by `0x08` (`0x0B` in some writers), `0x01` a NUL-terminated string, `0x02`
/// a little-endian int32 (`0x04` and `0x06` the same size), `0x03` a float32, `0x05` a
/// NUL-terminated UTF-16 string, `0x07` and `0x0A` eight bytes. Keys compare ignoring
/// ASCII case. Reading stops at the first byte that does not fit, keeping every
/// shortcut read whole until then.
pub fn shortcuts(bytes: &[u8]) -> Vec<Shortcut> {
    let mut r = Bytes(bytes);
    let mut out = Vec::new();
    // 1 inside `shortcuts`, 2 inside one shortcut, deeper in its `tags`.
    let mut depth = 0usize;
    let mut current = Shortcut::default();
    while let Some(kind) = r.byte() {
        if matches!(kind, 0x08 | 0x0B) {
            if depth == 2 && current != Shortcut::default() {
                out.push(std::mem::take(&mut current));
            }
            let Some(up) = depth.checked_sub(1) else {
                break;
            };
            depth = up;
            continue;
        }
        let Some(key) = r.cstr() else {
            break;
        };
        let field = depth == 2;
        match kind {
            0x00 => {
                depth += 1;
                if depth == 2 {
                    current = Shortcut::default();
                }
            }
            0x01 => {
                let Some(value) = r.cstr() else {
                    break;
                };
                if field && key.eq_ignore_ascii_case("exe") {
                    current.exe = unquote(&value);
                } else if field && key.eq_ignore_ascii_case("startdir") {
                    current.start_dir = unquote(&value);
                }
            }
            0x02 | 0x04 | 0x06 => {
                let Some(value) = r.take(4).and_then(|b| <[u8; 4]>::try_from(b).ok()) else {
                    break;
                };
                if field && key.eq_ignore_ascii_case("appid") {
                    current.app_id = Some(u32::from_le_bytes(value));
                }
            }
            0x03 => {
                if r.take(4).is_none() {
                    break;
                }
            }
            0x07 | 0x0A => {
                if r.take(8).is_none() {
                    break;
                }
            }
            0x05 => {
                if !r.skip_wide() {
                    break;
                }
            }
            _ => break,
        }
    }
    out
}

/// A shortcut's path value without the double quotes around it; `None` when empty.
fn unquote(value: &str) -> Option<String> {
    let value = value.trim();
    let value = value
        .strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
        .unwrap_or(value);
    (!value.is_empty()).then(|| value.to_owned())
}

/// A cursor over a binary VDF.
struct Bytes<'a>(&'a [u8]);

impl<'a> Bytes<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let (head, rest) = self.0.split_at_checked(n)?;
        self.0 = rest;
        Some(head)
    }

    fn byte(&mut self) -> Option<u8> {
        self.take(1).and_then(|b| b.first().copied())
    }

    /// A NUL-terminated string, decoded as UTF-8 with replacement characters.
    fn cstr(&mut self) -> Option<String> {
        let end = self.0.iter().position(|b| *b == 0)?;
        let text = String::from_utf8_lossy(self.take(end)?).into_owned();
        self.take(1)?;
        Some(text)
    }

    /// Skips a NUL-terminated UTF-16 string; `false` when it runs off the end.
    fn skip_wide(&mut self) -> bool {
        loop {
            match self.take(2) {
                Some([0, 0]) => return true,
                Some(_) => {}
                None => return false,
            }
        }
    }
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
        // Older files: a bare path under a numeric key; an app's size is no path.
        let old = "\"LibraryFolders\"\n{\n\t\"TimeNextStatsReport\"\t\t\"1600000000\"\n\t\"1\"\t\t\"/mnt/games/SteamLibrary\"\n\t\"apps\"\n\t{\n\t\t\"228980\"\t\t\"102931551\"\n\t}\n}\n";
        assert_eq!(library_paths(old), ["/mnt/games/SteamLibrary"]);
        assert!(library_paths("").is_empty());
        assert!(library_paths("\u{fffd}\0\"path\"").is_empty());
    }

    /// A binary `shortcuts.vdf` with one shortcut per `(appid, exe, start dir)`, laid out
    /// as Steam writes it: quoted paths, other fields around them, a `tags` map.
    fn vdf(shortcuts: &[(u32, &str, &str)]) -> Vec<u8> {
        let mut b = b"\x00shortcuts\x00".to_vec();
        for (i, (app_id, exe, dir)) in shortcuts.iter().enumerate() {
            b.extend(format!("\x00{i}\x00\x02appid\x00").as_bytes());
            b.extend(app_id.to_le_bytes());
            b.extend(b"\x01AppName\x00World of Warcraft\x00");
            b.extend(format!("\x01Exe\x00\"{exe}\"\x00\x01StartDir\x00\"{dir}\"\x00").as_bytes());
            b.extend(b"\x01LaunchOptions\x00\x00\x02IsHidden\x00\x00\x00\x00\x00");
            b.extend(b"\x00tags\x00\x010\x00favorite\x00\x08\x08");
        }
        b.extend(b"\x08\x08");
        b
    }

    fn shortcut(app_id: u32, exe: &str, dir: &str) -> Shortcut {
        Shortcut {
            app_id: Some(app_id),
            exe: Some(exe.into()),
            start_dir: Some(dir.into()),
        }
    }

    #[test]
    fn shortcuts_as_steam_writes_them() {
        let bytes = vdf(&[
            (
                0x8000_0001,
                "/g/World of Warcraft/_retail_/Wow.exe",
                "/g/World of Warcraft/_retail_/",
            ),
            (3_000_000_000, "/usr/bin/supertuxkart", "/usr/bin/"),
        ]);
        assert_eq!(
            shortcuts(&bytes),
            [
                shortcut(
                    2_147_483_649,
                    "/g/World of Warcraft/_retail_/Wow.exe",
                    "/g/World of Warcraft/_retail_/"
                ),
                shortcut(3_000_000_000, "/usr/bin/supertuxkart", "/usr/bin/"),
            ]
        );
    }

    #[test]
    fn old_lowercase_keys_and_every_value_type() {
        let mut b = b"\x00shortcuts\x00\x000\x00".to_vec();
        b.extend(b"\x03f\x00\x00\x00\x80\x3f\x05w\x00a\x00b\x00\x00\x00\x07u\x00\x01\x02\x03\x04\x05\x06\x07\x08");
        b.extend(b"\x0Ai\x00\x00\x00\x00\x00\x00\x00\x00\x00\x04p\x00\x01\x00\x00\x00\x06c\x00\xff\x00\x00\xff");
        b.extend(b"\x01exe\x00/g/wow/Wow.exe\x00\x01startdir\x00/g/wow\x00\x0B\x0B");
        assert_eq!(
            shortcuts(&b),
            [Shortcut {
                app_id: None,
                exe: Some("/g/wow/Wow.exe".into()),
                start_dir: Some("/g/wow".into()),
            }]
        );
    }

    #[test]
    fn a_cut_or_a_strange_byte_keeps_the_shortcuts_read_whole() {
        let bytes = vdf(&[(1, "/g/a/Wow.exe", "/g/a"), (2, "/g/b/Wow.exe", "/g/b")]);
        let whole = shortcuts(&bytes);
        assert_eq!(whole.len(), 2);
        for end in 0..bytes.len() {
            let got = shortcuts(&bytes[..end]);
            assert_eq!(got, whole[..got.len()], "cut at {end}");
        }
        // An unknown type after the first shortcut ends the reading there.
        let first = vdf(&[(1, "/g/a/Wow.exe", "/g/a")]);
        let mut odd = first[..first.len() - 2].to_vec();
        odd.extend(b"\x09x\x00");
        odd.extend(&bytes[first.len() - 2..]);
        assert_eq!(shortcuts(&odd), whole[..1]);
        assert!(shortcuts(b"\x08\x08\x08").is_empty());
        assert!(shortcuts(b"").is_empty());
    }

    #[test]
    fn only_client_exes_become_hints() {
        let home = Path::new("/h");
        let exe = |exe: &str, dir: Option<&str>| {
            Shortcut {
                app_id: None,
                exe: Some(exe.into()),
                start_dir: dir.map(str::to_owned),
            }
            .client_exe(home)
        };
        assert_eq!(
            exe("~/Games/wow/WowClassic.exe", None),
            Some(PathBuf::from("/h/Games/wow/WowClassic.exe"))
        );
        assert_eq!(
            exe("Wow.exe", Some("/g/wow")),
            Some(PathBuf::from("/g/wow/Wow.exe"))
        );
        assert_eq!(exe("Wow.exe", None), None);
        assert_eq!(exe("/g/Diablo IV.exe", Some("/g")), None);
        assert_eq!(Shortcut::default().client_exe(home), None);
    }

    proptest! {
        #[test]
        fn any_text_is_handled(text in any::<Vec<u8>>()) {
            let _ = library_paths(&String::from_utf8_lossy(&text));
        }

        #[test]
        fn any_bytes_are_handled(bytes in any::<Vec<u8>>()) {
            for s in shortcuts(&bytes) {
                let _ = s.client_exe(Path::new("/h"));
            }
        }
    }
}
