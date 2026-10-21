//! Filesystem hints every backup task reads instead of probing on its own: whether the
//! filesystem under a client's `<meta>` can clone files, and how much space is free
//! there. A hint is only that (M3-D30): whether a clone works is observed per attempt.
//! In debug builds `OMADDONS_FS_CLONE=yes|no` and `OMADDONS_FS_FREE_MB=<n>` override
//! them (DEC-7); release builds ignore both.

use std::path::Path;

/// The `statfs` magic numbers of filesystems that reflink files.
const CLONE_CAPABLE: &[i128] = &[
    0x9123_683E, // btrfs
    0x5846_5342, // xfs
];

/// Whether the filesystem is one that clones files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloneHint {
    Capable,
    /// ext4, tmpfs, FUSE, NTFS and anything unrecognised: byte copies only.
    Incapable,
}

/// Whether the filesystem holding `path` (or its nearest existing ancestor) clones.
pub fn clone_hint(path: &Path) -> CloneHint {
    let forced = seam(
        std::env::var_os("OMADDONS_FS_CLONE"),
        cfg!(debug_assertions),
    );
    match forced.as_deref() {
        Some("yes") => return CloneHint::Capable,
        Some("no") => return CloneHint::Incapable,
        _ => {}
    }
    let magic = rustix::fs::statfs(existing(path)).map(|s| i128::from(s.f_type));
    match magic {
        Ok(magic) if CLONE_CAPABLE.contains(&magic) => CloneHint::Capable,
        _ => CloneHint::Incapable,
    }
}

/// The bytes free to an unprivileged user on the filesystem holding `path` (or its
/// nearest existing ancestor): `f_bavail × f_frsize`, 0 when it cannot be read.
pub fn free_bytes(path: &Path) -> u64 {
    let forced = seam(
        std::env::var_os("OMADDONS_FS_FREE_MB"),
        cfg!(debug_assertions),
    );
    if let Some(mb) = forced.and_then(|v| v.parse::<u64>().ok()) {
        return mb.saturating_mul(1024 * 1024);
    }
    rustix::fs::statvfs(existing(path))
        .map(|s| s.f_bavail.saturating_mul(s.f_frsize))
        .unwrap_or(0)
}

/// A debug build's override from `var`; release builds ignore it.
fn seam(var: Option<std::ffi::OsString>, debug: bool) -> Option<String> {
    var.filter(|_| debug)?.into_string().ok()
}

/// `path`, or the nearest ancestor that exists (`<meta>` before the first backup).
fn existing(path: &Path) -> &Path {
    path.ancestors()
        .find(|p| std::fs::symlink_metadata(p).is_ok())
        .unwrap_or(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seams_apply_in_debug_builds_only() {
        assert_eq!(seam(Some("yes".into()), true).as_deref(), Some("yes"));
        assert_eq!(seam(Some("yes".into()), false), None);
        assert_eq!(seam(None, true), None);
    }

    #[test]
    fn hints_read_the_filesystem_or_the_nearest_ancestor() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("Interface/.omaddons/backups");
        assert_eq!(existing(&missing), dir.path());
        if std::env::var_os("OMADDONS_FS_FREE_MB").is_none() {
            assert!(free_bytes(&missing) > 0, "a temp dir has room");
        }
        if std::env::var_os("OMADDONS_FS_CLONE").is_none() {
            let magic = rustix::fs::statfs(dir.path()).unwrap().f_type;
            let expected = if CLONE_CAPABLE.contains(&i128::from(magic)) {
                CloneHint::Capable
            } else {
                CloneHint::Incapable
            };
            assert_eq!(clone_hint(&missing), expected, "magic {magic:#x}");
        }
        let target = Path::new(env!("CARGO_MANIFEST_DIR")).join("target");
        let magic = rustix::fs::statfs(&target).map(|s| i128::from(s.f_type));
        if magic == Ok(0x9123_683E) && std::env::var_os("OMADDONS_FS_CLONE").is_none() {
            assert_eq!(clone_hint(&target), CloneHint::Capable, "btrfs clones");
        }
    }
}
