//! The per-file index, `<state>/omaddons/targets/<key>.files.json`: size, mtime and
//! SHA-256 of every file of every managed folder, written by the engine from hashes
//! computed during extraction so that M2's drift check needs no backfill.
//!
//! Each folder carries the moment its entries were recorded (`recorded_at_ns`, M2-D16):
//! a file whose mtime is not safely before it may have changed in the same timestamp
//! tick, so the drift check re-hashes it ("racily clean"). The field is optional, so
//! the schema stays 1.0 and an M1 binary still reads and saves the file (M2-D36).
//! [`index_live_folder`] indexes a folder already on disk, for adoption.

use std::collections::BTreeMap;
use std::io::{self, Read};
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};

use super::store::{SchemaVersion, Versioned};
use crate::engine::inspect::ArchiveLimits;
use crate::error::{Error, ErrorCode, Result, Suggestion};
use crate::fsx::TreeDigest;
use crate::model::TargetKey;

/// The files of one target's managed folders.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FilesIndex {
    pub key: TargetKey,
    /// When the index was written: the racy-clean moment of folders without their own
    /// `recorded_at_ns` (indexes M1 wrote).
    pub written_at_ns: i128,
    /// Keyed by `FolderName::key()`.
    pub folders: BTreeMap<String, FolderIndex>,
}

impl Versioned for FilesIndex {
    const KIND: &'static str = "files_index";
    const CURRENT: SchemaVersion = SchemaVersion::new(1, 0);
}

impl FilesIndex {
    /// The moment `folder`'s entries are trusted from: its own stamp, else the index's
    /// write time (no worse than M1).
    pub fn recorded_at_ns(&self, folder: &FolderIndex) -> i128 {
        folder.recorded_at_ns.unwrap_or(self.written_at_ns)
    }
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
    /// When these entries were recorded: under the commit's lock, at least
    /// `RACY_SLACK_NS` after the newest staged mtime; at the end of a live walk.
    /// Absent in indexes M1 wrote ([`FilesIndex::recorded_at_ns`] falls back).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recorded_at_ns: Option<i128>,
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
            recorded_at_ns: None,
        }
    }

    /// The same entries, recorded at `at_ns`.
    #[must_use]
    pub fn stamped(mut self, at_ns: i128) -> Self {
        self.recorded_at_ns = Some(at_ns);
        self
    }

    /// The folder's tree digest, from its entries (what `tree` spells out).
    pub fn digest(&self) -> TreeDigest {
        TreeDigest::from_hex_entries(
            self.files
                .iter()
                .map(|f| (f.path.clone(), f.size, f.sha256.as_str())),
        )
    }
}

/// A file's mtime in nanoseconds, as every index entry records it.
pub fn mtime_ns(meta: &std::fs::Metadata) -> i128 {
    i128::from(meta.mtime()) * 1_000_000_000 + i128::from(meta.mtime_nsec())
}

/// Bytes read and lowercase hex SHA-256 of `reader`, in 64 KiB chunks.
fn hash_reader(mut reader: impl Read) -> io::Result<(u64, String)> {
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    let mut size = 0u64;
    loop {
        let n = match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        hasher.update(buf.get(..n).unwrap_or_default());
        size = size.saturating_add(n as u64);
    }
    let hex = hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    Ok((size, hex))
}

/// Bytes read and lowercase hex SHA-256 of the file at `path`, streamed: no hashing
/// path reads a whole file into memory (SavedVariables reach 64 MiB).
pub fn hash_file_streaming(path: &Path) -> io::Result<(u64, String)> {
    hash_reader(std::fs::File::open(path)?)
}

/// The index of the live folder at `path`, as a commit would record it: every regular
/// file (none skipped), its path relative to the folder (`/`-separated), size and mtime
/// from the `fstat` of the handle hashed, and the tree digest; stamped at the end of the
/// walk. Symlinks are never followed.
///
/// Fails `io` with `details.reason` `unindexable` (and the folder-relative `path`) on a
/// symlink, device, FIFO or socket inside, a name that is not UTF-8, a file that
/// changed while hashed or one that cannot be read; and `archive_too_large` with
/// `details.reason` `limit_exceeded` past `limits`' entry count or total size (adopting
/// a folder means reading it).
pub fn index_live_folder(path: &Path, limits: &ArchiveLimits) -> Result<FolderIndex> {
    let folder = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let unindexable = |rel: &str, why: &str| {
        Error::new(
            ErrorCode::Io,
            format!("{folder}/{rel} {why}, so the folder cannot be indexed"),
            Suggestion::text("replace it with a regular file, or leave the folder unmanaged"),
        )
        .details(json!({ "reason": "unindexable", "folder": folder, "path": rel }))
    };
    let unreadable = |rel: &str, e: &io::Error| unindexable(rel, &format!("cannot be read ({e})"));
    let over = |what: &str, measured: u64, limit: u64, key: &str| {
        Error::new(
            ErrorCode::ArchiveTooLarge,
            format!("{folder} has {measured} {what}, over the limit of {limit}"),
            Suggestion::text(format!(
                "if the folder is genuine, raise the limit: omaddons config set {key} <value>"
            )),
        )
        .details(json!({
            "reason": "limit_exceeded",
            "folder": folder,
            "measured": measured,
            "limit": limit,
            "config_key": key,
        }))
    };

    let meta = std::fs::symlink_metadata(path).map_err(|e| unreadable(".", &e))?;
    if !meta.is_dir() {
        return Err(unindexable(".", "is not a directory"));
    }
    let mut entries: Vec<FileEntry> = Vec::new();
    let mut total = 0u64;
    let mut dirs = vec![(path.to_path_buf(), String::new())];
    while let Some((dir, prefix)) = dirs.pop() {
        let listing = std::fs::read_dir(&dir).map_err(|e| unreadable(&prefix, &e))?;
        for item in listing {
            let item = item.map_err(|e| unreadable(&prefix, &e))?;
            let name = item.file_name();
            let Some(name) = name.to_str() else {
                let lossy = name.to_string_lossy();
                let rel = if prefix.is_empty() {
                    lossy.into_owned()
                } else {
                    format!("{prefix}/{lossy}")
                };
                return Err(unindexable(&rel, "has a name that is not UTF-8"));
            };
            let rel = if prefix.is_empty() {
                name.to_owned()
            } else {
                format!("{prefix}/{name}")
            };
            let kind = std::fs::symlink_metadata(item.path())
                .map_err(|e| unreadable(&rel, &e))?
                .file_type();
            if kind.is_dir() {
                dirs.push((item.path(), rel));
                continue;
            }
            if !kind.is_file() {
                return Err(unindexable(&rel, "is a symlink, device, FIFO or socket"));
            }
            let count = entries.len() as u64 + 1;
            if count > limits.max_entries {
                return Err(over(
                    "files",
                    count,
                    limits.max_entries,
                    "limits.archive_max_entries",
                ));
            }
            let file = std::fs::File::open(item.path()).map_err(|e| unreadable(&rel, &e))?;
            let stat = file.metadata().map_err(|e| unreadable(&rel, &e))?;
            if total.saturating_add(stat.len()) > limits.max_uncompressed {
                return Err(over(
                    "bytes",
                    total.saturating_add(stat.len()),
                    limits.max_uncompressed,
                    "limits.archive_max_uncompressed",
                ));
            }
            let (read, sha256) = hash_reader(&file).map_err(|e| unreadable(&rel, &e))?;
            // Rewritten in place at the same size shows in its mtime alone.
            let after = file.metadata().map_err(|e| unreadable(&rel, &e))?;
            if read != stat.len()
                || after.len() != stat.len()
                || mtime_ns(&after) != mtime_ns(&stat)
            {
                return Err(unindexable(&rel, "changed while it was read"));
            }
            total = total.saturating_add(read);
            entries.push(FileEntry {
                path: rel,
                size: read,
                mtime_ns: mtime_ns(&stat),
                sha256,
            });
        }
    }
    let recorded = jiff::Timestamp::now().as_nanosecond();
    Ok(FolderIndex::new(folder, entries).stamped(recorded))
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

    fn index_of(folders: Vec<FolderIndex>) -> FilesIndex {
        FilesIndex {
            key: "01JB8Q2V9W0000000000000000".parse().unwrap(),
            written_at_ns: 1_000,
            folders: folders
                .into_iter()
                .map(|f| (f.name.to_ascii_lowercase(), f))
                .collect(),
        }
    }

    #[test]
    fn the_stamp_is_optional_and_falls_back_to_the_write_time() {
        let plain = FolderIndex::new("Foo", vec![entry("Foo.toc", b"x")]);
        let stamped = FolderIndex::new("Bar", vec![entry("Bar.toc", b"y")]).stamped(700);
        let json = serde_json::to_value(&plain).unwrap();
        assert!(
            json.get("recorded_at_ns").is_none(),
            "absent, not null: {json}"
        );
        assert_eq!(
            serde_json::to_value(&stamped).unwrap()["recorded_at_ns"],
            700
        );
        let index = index_of(vec![plain.clone(), stamped.clone()]);
        assert_eq!(index.recorded_at_ns(&plain), 1_000, "M1: the write time");
        assert_eq!(index.recorded_at_ns(&stamped), 700);
    }

    /// The index as M1 declared it: no `recorded_at_ns`, unknown fields ignored.
    #[derive(Serialize, Deserialize)]
    struct FilesIndexM1 {
        key: TargetKey,
        written_at_ns: i128,
        folders: BTreeMap<String, FolderIndexM1>,
    }

    #[derive(Serialize, Deserialize)]
    struct FolderIndexM1 {
        name: String,
        tree: String,
        files: Vec<FileEntry>,
    }

    #[test]
    fn m1_reads_and_saves_an_m2_index_and_m2_reads_it_back() {
        let m2 = index_of(vec![
            FolderIndex::new("Foo", vec![entry("Foo.toc", b"x")]).stamped(700),
        ]);
        let file = crate::state::store::encode(&m2).unwrap();
        let text = String::from_utf8(file.clone()).unwrap();
        assert!(text.contains("\"schema_version\": \"1.0\""), "{text}");
        assert!(text.contains("recorded_at_ns"));

        // M1 decodes the same file (the store envelope's body is the struct) and saves it.
        let value: serde_json::Value = serde_json::from_slice(&file).unwrap();
        let m1: FilesIndexM1 = serde_json::from_value(value).unwrap();
        let saved = serde_json::to_value(&m1).unwrap();
        assert!(!saved.to_string().contains("recorded_at_ns"), "M1 drops it");

        // M2 reads M1's save: the stamp is gone, the fallback applies.
        let back: FilesIndex = serde_json::from_value(saved).unwrap();
        let foo = &back.folders["foo"];
        assert_eq!(foo.recorded_at_ns, None);
        assert_eq!(back.recorded_at_ns(foo), 1_000);
        assert_eq!(foo.tree, m2.folders["foo"].tree);
    }

    #[test]
    fn applying_changes_keeps_carried_stamps_and_other_folders() {
        use crate::engine::journal::IndexChanges;

        let old = FolderIndex::new("Old", vec![entry("Old.toc", b"o")]).stamped(100);
        let gone = FolderIndex::new("Gone", vec![entry("Gone.toc", b"g")]).stamped(200);
        let mut index = index_of(vec![old.clone(), gone]);
        let changes = IndexChanges {
            upsert: vec![
                FolderIndex::new("New", vec![entry("New.toc", b"n")]).stamped(900),
                FolderIndex::new("Adopted", vec![entry("A.toc", b"a")]).stamped(450),
            ],
            remove: vec!["gone".into()],
        };
        changes.apply(&mut index);
        let stamps: Vec<(&str, Option<i128>)> = index
            .folders
            .values()
            .map(|f| (f.name.as_str(), f.recorded_at_ns))
            .collect();
        assert_eq!(
            stamps,
            [
                ("Adopted", Some(450)),
                ("New", Some(900)),
                ("Old", Some(100))
            ]
        );
    }

    fn write(path: &Path, bytes: &[u8]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }

    #[test]
    fn a_live_folder_indexes_every_file_without_following_links() {
        let dir = tempfile::tempdir().unwrap();
        let foo = dir.path().join("Foo");
        write(&foo.join("Foo.toc"), b"## Interface: 120105");
        write(&foo.join("Libs/Ace/Ace.lua"), b"local A");
        write(&foo.join(".git/HEAD"), b"ref: main");
        std::fs::create_dir_all(foo.join("empty/deeper")).unwrap();
        let before = jiff::Timestamp::now().as_nanosecond();
        let index = index_live_folder(&foo, &ArchiveLimits::default()).unwrap();
        let paths: Vec<&str> = index.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(
            paths,
            [".git/HEAD", "Foo.toc", "Libs/Ace/Ace.lua"],
            "every regular file, none skipped; empty dirs leave no entry"
        );
        assert_eq!(index.name, "Foo");
        let toc = &index.files[1];
        assert_eq!(
            (toc.size, toc.sha256.as_str()),
            (20, hex(&sha(b"## Interface: 120105")).as_str())
        );
        let stat = std::fs::metadata(foo.join("Foo.toc")).unwrap();
        assert_eq!(toc.mtime_ns, mtime_ns(&stat));
        assert_eq!(
            index.tree,
            FolderIndex::new("Foo", index.files.clone()).tree
        );
        assert!(index.recorded_at_ns.is_some_and(|at| at >= before));

        // An inner symlink makes the folder unindexable.
        std::os::unix::fs::symlink("/etc/passwd", foo.join("Libs/link.lua")).unwrap();
        let err = index_live_folder(&foo, &ArchiveLimits::default()).unwrap_err();
        assert_eq!(err.code, ErrorCode::Io);
        assert_eq!(
            (&err.details["reason"], &err.details["path"]),
            (&json!("unindexable"), &json!("Libs/link.lua"))
        );
        // The folder itself must be a real directory.
        let linked = dir.path().join("Linked");
        std::os::unix::fs::symlink(&foo, &linked).unwrap();
        let err = index_live_folder(&linked, &ArchiveLimits::default()).unwrap_err();
        assert_eq!(err.details["reason"], "unindexable");
    }

    #[test]
    fn a_live_folder_past_a_limit_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let foo = dir.path().join("Foo");
        write(&foo.join("a.lua"), &[b'a'; 600]);
        write(&foo.join("b.lua"), &[b'b'; 600]);
        let limits = |max_entries, max_uncompressed| ArchiveLimits {
            max_entries,
            max_uncompressed,
            ..ArchiveLimits::default()
        };
        assert!(
            index_live_folder(&foo, &limits(2, 1_200)).is_ok(),
            "at the limits"
        );
        for (limits, key) in [
            (limits(1, 10_000), "limits.archive_max_entries"),
            (limits(10, 1_000), "limits.archive_max_uncompressed"),
        ] {
            let err = index_live_folder(&foo, &limits).unwrap_err();
            assert_eq!(err.code, ErrorCode::ArchiveTooLarge);
            assert_eq!(
                (&err.details["reason"], &err.details["config_key"]),
                (&json!("limit_exceeded"), &json!(key))
            );
        }
    }

    #[test]
    fn large_files_hash_by_streaming() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("SavedVariables.lua");
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(200 * 1024 * 1024).unwrap();
        let (size, hex) = hash_file_streaming(&path).unwrap();
        assert_eq!(size, 200 * 1024 * 1024);
        // `head -c 209715200 /dev/zero | sha256sum`
        assert_eq!(
            hex,
            "72abf2ca8f36943ebe2e49ca3a51d409ca5f0bfcffab6c9d25643c17c32889da"
        );
    }
}
