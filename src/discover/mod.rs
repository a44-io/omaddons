//! Client discovery: launcher and Battle.net hints, the bounded scan, `.build.info`,
//! `.flavor.info`, `product.db`, `dosdevices`, PE versions, realmlist labels, running
//! clients, and the target registry.
//!
//! Discovery only reads. Every file it reads is client metadata outside omaddons'
//! control, so reads are bounded by [`DISCOVERY_FILE_MAX_BYTES`] and decoded as lossy
//! UTF-8, and a missing or unreadable file is an absent hint, never an error.

pub mod battlenet;
pub mod bpsv;
pub mod config_wtf;
pub mod dosdevices;
pub mod product_db;
pub mod target;

use std::io::Read;
use std::path::{Path, PathBuf};

pub use target::{DiscoveredTarget, DiscoverySource};

use crate::limits::DISCOVERY_FILE_MAX_BYTES;

/// The first [`DISCOVERY_FILE_MAX_BYTES`] of a regular file as lossy UTF-8; `None` when
/// it is missing, not a file or unreadable.
pub(crate) fn read_text(path: &Path) -> Option<String> {
    let file = std::fs::File::open(path).ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(DISCOVERY_FILE_MAX_BYTES)
        .read_to_end(&mut bytes)
        .ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// The bytes of a regular file of at most [`DISCOVERY_FILE_MAX_BYTES`]; `None` when it
/// is missing, not a file, unreadable or larger (a binary format is not cut short).
pub(crate) fn read_bytes(path: &Path) -> Option<Vec<u8>> {
    let file = std::fs::File::open(path).ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(DISCOVERY_FILE_MAX_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)
        .ok()?;
    (bytes.len() as u64 <= DISCOVERY_FILE_MAX_BYTES).then_some(bytes)
}

/// `dir/name`, matched ignoring ASCII case when the exact name is absent: Wine
/// prefixes are case-sensitive on disk, but clients and launchers are not.
pub(crate) fn find_child(dir: &Path, name: &str) -> Option<PathBuf> {
    let exact = dir.join(name);
    if exact.symlink_metadata().is_ok() {
        return Some(exact);
    }
    let mut matches: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(|n| n.eq_ignore_ascii_case(name))
        })
        .map(|entry| entry.path())
        .collect();
    matches.sort();
    matches.into_iter().next()
}

/// `dir` joined with each component of `rel`, each matched by [`find_child`].
pub(crate) fn find_path(dir: &Path, rel: &[&str]) -> Option<PathBuf> {
    rel.iter()
        .try_fold(dir.to_path_buf(), |at, part| find_child(&at, part))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn reads_are_bounded_lossy_and_optional() {
        let dir = tempfile::tempdir().unwrap();
        let big = dir.path().join("big");
        fs::write(&big, vec![b'a'; DISCOVERY_FILE_MAX_BYTES as usize + 10]).unwrap();
        assert_eq!(
            read_text(&big).unwrap().len() as u64,
            DISCOVERY_FILE_MAX_BYTES
        );
        let bad = dir.path().join("bad");
        fs::write(&bad, b"ok\xffok").unwrap();
        assert_eq!(read_text(&bad).unwrap(), "ok\u{fffd}ok");
        assert_eq!(read_text(&dir.path().join("missing")), None);
        assert_eq!(read_text(dir.path()), None, "a directory is not a file");
        assert_eq!(
            read_bytes(&big),
            None,
            "binary reads refuse oversized files"
        );
        let exact = dir.path().join("exact");
        fs::write(&exact, vec![1u8; DISCOVERY_FILE_MAX_BYTES as usize]).unwrap();
        assert_eq!(
            read_bytes(&exact).map(|b| b.len() as u64),
            Some(DISCOVERY_FILE_MAX_BYTES)
        );
    }

    #[test]
    fn children_match_ignoring_ascii_case() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("wtf")).unwrap();
        fs::write(dir.path().join("wtf").join("CONFIG.WTF"), "x").unwrap();
        assert_eq!(
            find_path(dir.path(), &["WTF", "Config.wtf"]),
            Some(dir.path().join("wtf").join("CONFIG.WTF"))
        );
        assert_eq!(find_child(dir.path(), "nope"), None);
        fs::write(dir.path().join("WTF"), "exact wins").unwrap();
        assert_eq!(find_child(dir.path(), "WTF"), Some(dir.path().join("WTF")));
    }
}
