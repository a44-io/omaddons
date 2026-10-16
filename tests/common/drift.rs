//! Drift mutators: what a user (or another manager) does to managed files between
//! commands. Tests may write directly: the single-writer rule covers `src/` only.

use std::fs;
use std::path::Path;
use std::time::Duration;

/// Different bytes at the same path; `file` must exist and `contents` must differ.
pub fn edit_managed(file: &Path, contents: &[u8]) {
    let before = fs::read(file).unwrap();
    assert_ne!(
        before,
        contents,
        "{} already holds these bytes",
        file.display()
    );
    fs::write(file, contents).unwrap();
}

/// Deletes a managed file, or a whole folder.
pub fn delete_managed(path: &Path) {
    if fs::symlink_metadata(path).unwrap().is_dir() {
        fs::remove_dir_all(path).unwrap();
    } else {
        fs::remove_file(path).unwrap();
    }
}

/// A file the archive never had, at `rel` inside `folder`.
pub fn add_stray_file(folder: &Path, rel: &str, contents: &[u8]) {
    let path = folder.join(rel);
    assert!(!path.exists(), "{} exists already", path.display());
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

/// Rewrites `file` with its own bytes and moves its mtime 2 s past the old one, so a
/// stat-first drift check must re-hash it, and find it clean.
pub fn touch_identical(file: &Path) {
    let bytes = fs::read(file).unwrap();
    let old = fs::metadata(file).unwrap().modified().unwrap();
    fs::write(file, &bytes).unwrap();
    let handle = fs::OpenOptions::new().write(true).open(file).unwrap();
    handle.set_modified(old + Duration::from_secs(2)).unwrap();
}
