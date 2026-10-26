//! A path's identity, compared the reboot-safe way: why the device is recorded but never
//! compared. A rename on one filesystem keeps a path's inode number and, where the
//! filesystem records one, its birth time; the device number does not survive a mount
//! (btrfs subvolumes, FUSE and device-mapper volumes get a new one every time). Recovery
//! decides whether a journaled step ran by identity, and fsx's endpoint predicates
//! compare it the same way, so the rule lives here, in the lower layer.

use std::os::unix::fs::MetadataExt;
use std::path::Path;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// A path's identity, which a rename on one filesystem keeps: its inode number and, where
/// the filesystem keeps one, its birth time. The device number is recorded for diagnosis
/// only: btrfs subvolumes, FUSE and device-mapper volumes get a new one on every mount,
/// so a reboot would make every step look like neither state. Compare with
/// [`FileId::same`], never `==`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct FileId {
    pub dev: u64,
    pub ino: u64,
    /// Nanoseconds since the epoch (statx `STATX_BTIME`); `None` where the filesystem
    /// has none, and in journals written before it was recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub born_ns: Option<i64>,
}

impl FileId {
    /// The identity of what is at `path` (not following a symlink), if anything.
    pub fn of(path: &Path) -> Option<Self> {
        std::fs::symlink_metadata(path)
            .ok()
            .map(|m| Self::from_metadata(&m))
    }

    /// The identity `meta` (read without following a symlink) describes.
    pub(crate) fn from_metadata(meta: &std::fs::Metadata) -> Self {
        Self {
            dev: meta.dev(),
            ino: meta.ino(),
            born_ns: meta
                .created()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .and_then(|d| i64::try_from(d.as_nanos()).ok()),
        }
    }

    /// Whether two recorded identities name the same file: both absent, or the same inode
    /// with the same birth time when both know it. The device is ignored: a step's two
    /// paths are on one filesystem by construction, and its number changes across mounts.
    pub fn same(a: Option<Self>, b: Option<Self>) -> bool {
        match (a, b) {
            (None, None) => true,
            (Some(a), Some(b)) => {
                // ubs:ignore — comparing inode numbers, not secrets
                a.ino == b.ino
                    && match (a.born_ns, b.born_ns) {
                        // ubs:ignore — comparing birth times, not secrets
                        (Some(x), Some(y)) => x == y,
                        _ => true,
                    }
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    const fn id(dev: u64, ino: u64, born_ns: Option<i64>) -> FileId {
        FileId { dev, ino, born_ns }
    }

    #[test]
    fn same_decides_by_inode_and_birth_time_never_device() {
        let a = id(7, 42, Some(1_700_000_000_123_456_789));
        for (case, x, y, want) in [
            ("both_absent", None, None, true),
            ("first_absent", None, Some(a), false),
            ("second_absent", Some(a), None, false),
            ("same_inode_and_birth_time", Some(a), Some(a), true),
            (
                "same_inode_other_birth_time",
                Some(a),
                Some(id(7, 42, Some(1))),
                false,
            ),
            (
                "birth_time_unknown_on_one_side",
                Some(a),
                Some(id(7, 42, None)),
                true,
            ),
            ("other_inode", Some(a), Some(id(7, 43, a.born_ns)), false),
            (
                "other_device_same_inode_and_birth_time",
                Some(a),
                Some(id(7 ^ 0x5a5a, 42, a.born_ns)),
                true,
            ),
        ] {
            let got = FileId::same(x, y);
            assert_eq!(
                got, want,
                "{case}: same({x:?}, {y:?}) = {got}, expected {want}"
            );
        }
    }

    #[test]
    fn of_reads_the_entry_not_the_link_target() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target");
        fs::write(&target, "x").unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let (of_link, of_target) = (FileId::of(&link), FileId::of(&target));
        assert!(of_link.is_some() && of_target.is_some());
        assert!(
            !FileId::same(of_link, of_target),
            "{of_link:?} vs {of_target:?}"
        );
    }

    #[test]
    fn of_an_absent_path_is_none() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(FileId::of(&dir.path().join("absent")), None);
    }

    #[test]
    fn from_metadata_equals_of() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("file");
        fs::write(&file, "x").unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&file, &link).unwrap();
        for (case, path) in [
            ("a file", &file),
            ("a directory", &dir.path().to_path_buf()),
            ("a symlink", &link),
        ] {
            let meta = fs::symlink_metadata(path).unwrap();
            assert_eq!(
                Some(FileId::from_metadata(&meta)),
                FileId::of(path),
                "{case}"
            );
        }
    }

    #[test]
    fn serializes_byte_for_byte_as_the_journal_did() {
        let bare = id(7, 42, None);
        let json = serde_json::to_string(&bare).unwrap();
        assert_eq!(json, r#"{"dev":7,"ino":42}"#, "without born_ns: {json}");
        let born = id(7, 42, Some(1_700_000_000_123_456_789));
        let json = serde_json::to_string(&born).unwrap();
        assert_eq!(
            json, r#"{"dev":7,"ino":42,"born_ns":1700000000123456789}"#,
            "with born_ns: {json}"
        );
        let legacy: FileId = serde_json::from_str(r#"{"dev":7,"ino":42}"#).unwrap();
        assert_eq!(legacy, bare, "a legacy line without born_ns");
        let schema = schemars::schema_for!(FileId);
        assert_eq!(schema.get("title").and_then(|t| t.as_str()), Some("FileId"));
    }
}
