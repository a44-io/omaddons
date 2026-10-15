//! Addon folders and TOC text for TOC, inventory and engine tests.

use std::fs;
use std::path::{Path, PathBuf};

/// `<addons_dir>/<name>/` with each TOC file (name, contents) and each other file
/// (relative path, bytes); returns the folder.
pub fn addon_folder(
    addons_dir: &Path,
    name: &str,
    tocs: &[(&str, &str)],
    files: &[(&str, &[u8])],
) -> PathBuf {
    let dir = addons_dir.join(name);
    fs::create_dir_all(&dir).unwrap();
    for (toc, contents) in tocs {
        fs::write(dir.join(toc), contents).unwrap();
    }
    for (rel, bytes) in files {
        let path = dir.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }
    dir
}

/// A TOC: `## Interface: <interface>` (a number or a comma list), then each extra line
/// verbatim (directives such as `## Title: X`, or file names).
pub fn toc(interface: &str, extra: &[&str]) -> String {
    let mut text = format!("## Interface: {interface}\n");
    for line in extra {
        text.push_str(line);
        text.push('\n');
    }
    text
}
