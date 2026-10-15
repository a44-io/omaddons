//! The single-writer rule (D26) as a test: outside `src/fsx/`, production code in `src/`
//! may read the filesystem but never write, rename, remove, create or chmod anything,
//! and only an explicit allowlist of modules may call `prune()`.
//!
//! The scanner is plain text matching. A false positive is fixed by moving the code into
//! fsx, never by weakening a pattern; an exception needs a vision-bead comment. Test code
//! is not production code (DEC-8): scanning stops at a top-level `#[cfg(test)]`, and
//! files marked `#![cfg(test)]` are skipped.

#[cfg(test)]
mod scanner {
    use std::path::{Path, PathBuf};

    /// Write-side calls that may appear only in `src/fsx/`.
    const PATTERNS: &[&str] = &[
        "fs::write(",
        "fs::rename(",
        "fs::remove_file(",
        "fs::remove_dir(",
        "fs::remove_dir_all(",
        "fs::create_dir(",
        "fs::create_dir_all(",
        "fs::copy(",
        "fs::hard_link(",
        "fs::set_permissions(",
        "fs::soft_link(",
        "os::unix::fs::symlink(",
        "File::create(",
        "File::create_new(",
        "OpenOptions",
        ".set_len(",
        "tempfile::",
        ".persist(",
        "reflink_copy::",
    ];

    /// rustix calls that write when they appear with `rustix::fs::`.
    const RUSTIX_WRITES: &[&str] = &[
        "renameat",
        "unlinkat",
        "mkdirat",
        "fchmod",
        "symlinkat",
        "linkat",
    ];

    /// Names that may not be imported from `std::fs` outside fsx.
    const FS_IMPORTS: &[&str] = &[
        "write",
        "rename",
        "remove_file",
        "remove_dir",
        "remove_dir_all",
        "create_dir",
        "create_dir_all",
        "copy",
        "hard_link",
        "set_permissions",
        "File",
        "OpenOptions",
    ];

    /// `line` with comments dropped and the contents of `"…"` literals blanked.
    pub fn code_of(line: &str) -> String {
        let trimmed = line.trim_start();
        if trimmed.starts_with("//") {
            return String::new();
        }
        let mut out = String::new();
        let mut in_string = false;
        let mut escaped = false;
        let mut chars = line.chars().peekable();
        while let Some(c) = chars.next() {
            if in_string {
                if escaped {
                    escaped = false;
                } else if c == '\\' {
                    escaped = true;
                } else if c == '"' {
                    in_string = false;
                    out.push('"');
                }
                continue;
            }
            if c == '/' && chars.peek() == Some(&'/') {
                break;
            }
            if c == '"' {
                in_string = true;
            }
            out.push(c);
        }
        out
    }

    /// Every violation in one file, as `label:line: pattern`.
    pub fn scan_source(label: &str, text: &str) -> Vec<String> {
        if text.lines().any(|line| line.trim() == "#![cfg(test)]") {
            return Vec::new();
        }
        let mut found = Vec::new();
        let mut fs_import: Option<String> = None;
        for (index, line) in text.lines().enumerate() {
            if line.starts_with("#[cfg(test)]") {
                break;
            }
            let code = code_of(line);
            let at = |what: &str| format!("{label}:{}: {what}", index + 1);
            for pattern in PATTERNS {
                if code.contains(pattern) {
                    found.push(at(pattern));
                }
            }
            if code.contains("rustix::fs::") {
                for call in RUSTIX_WRITES {
                    if code.contains(call) {
                        found.push(at(&format!("rustix::fs::{call}")));
                    }
                }
            }
            let trimmed = code.trim();
            if trimmed.starts_with("use std::fs as") {
                found.push(at("use std::fs as"));
            }
            if let Some(rest) = trimmed.strip_prefix("use std::fs::") {
                fs_import = Some(rest.to_owned());
            } else if let Some(list) = fs_import.as_mut() {
                list.push_str(trimmed);
            }
            if let Some(list) = fs_import.clone()
                && list.contains(';')
            {
                let names = list.trim_matches(|c| c == '{' || c == '}' || c == ';');
                for name in names.split([',', '{', '}', ';']).map(str::trim) {
                    let name = name.split(" as ").next().unwrap_or("").trim();
                    if FS_IMPORTS.contains(&name) {
                        found.push(at(&format!("use std::fs::{name}")));
                    }
                }
                fs_import = None;
            }
        }
        found
    }

    /// Every `*.rs` file under `dir`.
    pub fn rust_files(dir: &Path) -> Vec<PathBuf> {
        let mut files = Vec::new();
        let mut pending = vec![dir.to_path_buf()];
        while let Some(dir) = pending.pop() {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    pending.push(path);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    files.push(path);
                }
            }
        }
        files.sort();
        files
    }

    pub fn root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
    }

    pub fn label(path: &Path) -> String {
        path.strip_prefix(root()).unwrap().display().to_string()
    }
}

use scanner::{code_of, label, root, rust_files, scan_source};

#[test]
fn the_scanner_flags_writes_and_ignores_reads_comments_and_strings() {
    let flagged = scan_source(
        "src/app/x.rs",
        "fn f() {\n    std::fs::write(\"x\", b\"y\").unwrap();\n}\n",
    );
    assert_eq!(flagged, ["src/app/x.rs:2: fs::write("]);
    for (source, expected) in [
        (
            "let s = fs::read_to_string(p)?; let m = fs::metadata(p)?; File::open(p)?;",
            0,
        ),
        ("// fs::remove_file(p) in a comment", 0),
        ("let doc = \"call fs::remove_dir_all( here\";", 0),
        ("let x = 1; // then fs::rename(a, b)", 0),
        ("use std::fs as f;", 1),
        ("use std::fs::{read, write};", 1),
        ("use std::fs::{\n    read_dir,\n    OpenOptions,\n};", 2),
        ("use std::fs::File;", 1),
        ("rustix::fs::renameat_with(a, b, c, d, e)", 1),
        ("let t = tempfile::tempdir()?;", 1),
        ("file.set_len(0)?;", 1),
        (
            "fn ok() {}\n#[cfg(test)]\nmod tests { fn t() { std::fs::write(\"a\", \"b\").unwrap(); } }",
            0,
        ),
        (
            "#![cfg(test)]\nfn t() { std::fs::remove_file(\"a\").unwrap(); }",
            0,
        ),
    ] {
        assert_eq!(
            scan_source("src/x.rs", source).len(),
            expected,
            "{source:?}"
        );
    }
    assert_eq!(code_of("  // all comment"), "");
}

#[test]
fn only_fsx_writes_to_the_filesystem() {
    let fsx = root().join("src/fsx");
    let mut violations = Vec::new();
    for file in rust_files(&root().join("src")) {
        if file.starts_with(&fsx) {
            continue;
        }
        let text = std::fs::read_to_string(&file).unwrap();
        violations.extend(scan_source(&label(&file), &text));
    }
    assert!(
        violations.is_empty(),
        "filesystem writes outside src/fsx/ (move them into fsx):\n{}",
        violations.join("\n")
    );
}

/// Modules allowed to call `prune()`, each with its reason. Doctor fixers must never
/// appear here (vision §5.5): the doctor removes by renaming into quarantine.
const PRUNE_CALLERS: &[(&str, &str)] = &[
    (
        "src/engine/tx.rs",
        "post-commit cleanup of stage dirs and git temp checkouts",
    ),
    (
        "src/engine/journal.rs",
        "committed journals, at finish and in recovery",
    ),
    (
        "src/engine/retention.rs",
        "snapshots, and the orphan sweep of stage dirs and in-flight locks",
    ),
    (
        "src/engine/blobs.rs",
        "blob eviction, and temp blobs that lost a same-hash race",
    ),
];

#[test]
fn prune_is_called_only_from_its_allowlist() {
    let allowed: Vec<&str> = PRUNE_CALLERS.iter().map(|(file, _)| *file).collect();
    assert!(!allowed.iter().any(|file| file.contains("doctor")));
    let mut calls = Vec::new();
    for file in rust_files(&root().join("src")) {
        let name = label(&file);
        if name == "src/fsx/prune.rs" {
            continue;
        }
        let text = std::fs::read_to_string(&file).unwrap();
        if text.lines().any(|line| line.trim() == "#![cfg(test)]") {
            continue;
        }
        for (index, line) in text.lines().enumerate() {
            if line.starts_with("#[cfg(test)]") {
                break;
            }
            let code = code_of(line);
            let calls_prune = code.match_indices("prune(").any(|(at, _)| {
                code.get(..at)
                    .and_then(|before| before.chars().last())
                    .is_none_or(|c| !(c.is_alphanumeric() || c == '_'))
            });
            if calls_prune && !allowed.contains(&name.as_str()) {
                calls.push(format!("{name}:{}", index + 1));
            }
        }
    }
    assert!(
        calls.is_empty(),
        "prune() called outside the allowlist:\n{}",
        calls.join("\n")
    );
}
