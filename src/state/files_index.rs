//! The per-file index, `<state>/omaddons/targets/<key>.files.json`: size, mtime and
//! SHA-256 of every file of every managed folder, written by the engine from hashes
//! computed during extraction so that M2's drift check needs no backfill.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::store::{SchemaVersion, Versioned};
use crate::fsx::TreeDigest;
use crate::model::TargetKey;

/// The files of one target's managed folders.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FilesIndex {
    pub key: TargetKey,
    /// When the index was written. M2's drift check re-hashes "racily clean" entries
    /// whose mtime is at or after this instant.
    pub written_at_ns: i128,
    /// Keyed by `FolderName::key()`.
    pub folders: BTreeMap<String, FolderIndex>,
}

impl Versioned for FilesIndex {
    const KIND: &'static str = "files_index";
    const CURRENT: SchemaVersion = SchemaVersion::new(1, 0);
}

/// One folder's files.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FolderIndex {
    /// The folder's on-disk spelling.
    pub name: String,
    /// `tree:sha256:<hex>`, fsx's digest of these entries.
    pub tree: String,
    /// Sorted by path.
    pub files: Vec<FileEntry>,
}

/// One file, by its path relative to the folder (`/`-separated).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FileEntry {
    pub path: String,
    pub size: u64,
    pub mtime_ns: i128,
    /// Lowercase hex.
    pub sha256: String,
}

impl FolderIndex {
    /// Sorts `files` by path and computes the tree digest.
    pub fn new(name: impl Into<String>, mut files: Vec<FileEntry>) -> Self {
        files.sort_by(|a, b| a.path.as_bytes().cmp(b.path.as_bytes()));
        let tree = TreeDigest::from_hex_entries(
            files
                .iter()
                .map(|f| (f.path.clone(), f.size, f.sha256.as_str())),
        );
        Self {
            name: name.into(),
            tree: tree.to_string(),
            files,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    fn sha(bytes: &[u8]) -> [u8; 32] {
        Sha256::digest(bytes).into()
    }

    fn hex(bytes: &[u8; 32]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    fn entry(path: &str, content: &[u8]) -> FileEntry {
        FileEntry {
            path: path.into(),
            size: content.len() as u64,
            mtime_ns: 1_760_000_000_000_000_000,
            sha256: hex(&sha(content)),
        }
    }

    #[test]
    fn entries_sort_by_path_and_the_digest_matches_fsx() {
        let files = vec![
            entry("Libs/Ace.lua", b"local A"),
            entry("Foo.toc", b"## Interface: 120105"),
        ];
        let folder = FolderIndex::new("Foo", files.clone());
        let paths: Vec<&str> = folder.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, ["Foo.toc", "Libs/Ace.lua"]);

        let fsx = TreeDigest::from_entries([
            ("Libs/Ace.lua", 7, sha(b"local A")),
            ("Foo.toc", 20, sha(b"## Interface: 120105")),
        ]);
        assert_eq!(folder.tree, fsx.to_string());
        assert!(folder.tree.starts_with("tree:sha256:"));

        let reversed = FolderIndex::new("Foo", files.into_iter().rev().collect());
        assert_eq!(
            reversed.tree, folder.tree,
            "insertion order does not matter"
        );
    }

    #[test]
    fn the_index_serializes_with_written_at_ns() {
        let index = FilesIndex {
            key: "01JB8Q2V9W0000000000000000".parse().unwrap(),
            written_at_ns: 1_760_000_000_000_000_001,
            folders: BTreeMap::from([(
                "foo".into(),
                FolderIndex::new("Foo", vec![entry("Foo.toc", b"x")]),
            )]),
        };
        let json = serde_json::to_value(&index).unwrap();
        assert_eq!(json["written_at_ns"], 1_760_000_000_000_000_001_i64);
        assert_eq!(serde_json::from_value::<FilesIndex>(json).unwrap(), index);
    }
}
