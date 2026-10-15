//! Windows paths in a Wine prefix to Linux paths, through the prefix's
//! `dosdevices/<letter>:` symlinks (`c: → ../drive_c`, `z: → /`, …). Wine resolves
//! names ignoring case, so each component is matched the same way. Reads only.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use super::find_child;

/// Where drive `letter` of `prefix` points; `None` without a mapping. A relative link
/// (`../drive_c`) is resolved lexically against `dosdevices/`, so `c:` maps to
/// `<prefix>/drive_c`.
fn drive(prefix: &Path, letter: char) -> Option<PathBuf> {
    let dosdevices = prefix.join("dosdevices");
    let link = dosdevices.join(format!("{}:", letter.to_ascii_lowercase()));
    let target = std::fs::read_link(link).ok()?;
    if target.is_absolute() {
        return Some(target);
    }
    let mut at = dosdevices;
    for component in target.components() {
        match component {
            Component::ParentDir => {
                at.pop();
            }
            Component::CurDir => {}
            other => at.push(other),
        }
    }
    Some(at)
}

/// The Linux path of Windows path `win` (`C:/Program Files (x86)/World of Warcraft`,
/// backslashes allowed) inside `prefix`; `None` when the drive letter has no mapping or
/// a component does not exist.
pub fn map_windows_path(prefix: &Path, win: &str) -> Option<PathBuf> {
    let win = win.replace('\\', "/");
    let mut chars = win.chars();
    let letter = chars.next().filter(char::is_ascii_alphabetic)?;
    if chars.next() != Some(':') {
        return None;
    }
    let mut at = drive(prefix, letter)?;
    for part in chars
        .as_str()
        .split('/')
        .filter(|p| !p.is_empty() && *p != ".")
    {
        if part == ".." {
            return None;
        }
        at = find_child(&at, part)?;
    }
    Some(at)
}

/// Every mapped drive of `prefix`, for diagnostics and for mapping process paths.
pub fn drive_map(prefix: &Path) -> BTreeMap<char, PathBuf> {
    ('a'..='z')
        .filter_map(|letter| drive(prefix, letter).map(|path| (letter, path)))
        .collect()
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::symlink;

    use super::*;

    #[test]
    fn windows_paths_map_through_dosdevices() {
        let dir = tempfile::tempdir().unwrap();
        let prefix = dir.path().join("prefix");
        let elsewhere = dir.path().join("elsewhere");
        fs::create_dir_all(prefix.join("dosdevices")).unwrap();
        fs::create_dir_all(prefix.join("drive_c/program files (x86)/World of Warcraft/_retail_"))
            .unwrap();
        fs::create_dir_all(elsewhere.join("Games/WoW")).unwrap();
        symlink("../drive_c", prefix.join("dosdevices/c:")).unwrap();
        symlink(&elsewhere, prefix.join("dosdevices/d:")).unwrap();
        symlink("/", prefix.join("dosdevices/z:")).unwrap();

        let wow = prefix.join("drive_c/program files (x86)/World of Warcraft");
        assert_eq!(
            map_windows_path(&prefix, "C:/Program Files (x86)/World of Warcraft"),
            Some(wow.clone()),
            "relative link, case-insensitive component"
        );
        assert_eq!(
            map_windows_path(&prefix, "c:\\PROGRAM FILES (X86)\\world of warcraft\\"),
            Some(wow.clone()),
            "backslashes, lowercase letter, trailing separator"
        );
        assert!(
            map_windows_path(&prefix, "C:/Program Files (x86)/World of Warcraft/_retail_")
                .is_some()
        );
        assert_eq!(
            map_windows_path(&prefix, "D:/Games/WoW"),
            Some(elsewhere.join("Games/WoW")),
            "absolute link outside drive_c"
        );
        assert_eq!(map_windows_path(&prefix, "Z:/"), Some(PathBuf::from("/")));
        assert_eq!(
            map_windows_path(&prefix, "E:/Games"),
            None,
            "unmapped letter"
        );
        assert_eq!(map_windows_path(&prefix, "C:/Missing"), None);
        assert_eq!(
            map_windows_path(&prefix, "C:/../etc"),
            None,
            "no climbing out"
        );
        assert_eq!(
            map_windows_path(&prefix, "/home"),
            None,
            "not a Windows path"
        );
        assert_eq!(map_windows_path(&prefix, ""), None);

        let map = drive_map(&prefix);
        assert_eq!(map.keys().copied().collect::<Vec<_>>(), ['c', 'd', 'z']);
        assert_eq!(map[&'c'], prefix.join("drive_c"));
        assert_eq!(map[&'d'], elsewhere);
    }
}
