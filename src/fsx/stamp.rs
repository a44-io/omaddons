//! Stat stamps (D24): what `lstat` says about a file, or about every entry of a
//! directory tree. The engine hashes and clones large files before it takes the global
//! lock; under the lock, an equal stamp shows that nothing was written since, so nothing
//! needs reading again. Read-only: they guard the digests next to them.
//!
//! `ctime` moves on every write, truncation, rename and chmod, and user space cannot set
//! it, so it catches what a preserved mtime would hide. It cannot catch a change in the
//! same tick of the kernel's file-timestamp clock as the stamp: a stamp is trusted only
//! once its `ctime` is a racy slack older than the moment it was read
//! ([`FileStamp::settled`]).

use std::fs::{self, FileType, Metadata};
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use crate::limits::{RACY_SLACK_COARSE_NS, RACY_SLACK_NS};

/// One path's identity and last change, symlinks not followed. Two equal stamps of a
/// settled file mean its bytes did not change in between.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileStamp {
    dev: u64,
    ino: u64,
    file_type: FileType,
    /// Permission bits.
    mode: u32,
    size: u64,
    mtime_ns: i128,
    ctime_ns: i128,
}

impl FileStamp {
    /// The stamp of whatever is at `path`; `None` when nothing is.
    pub fn read(path: &Path) -> io::Result<Option<Self>> {
        match fs::symlink_metadata(path) {
            Ok(meta) => Ok(Some(Self::of(&meta))),
            // ubs:ignore — an io::ErrorKind, not a secret
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(err),
        }
    }

    fn of(meta: &Metadata) -> Self {
        let ns = |secs: i64, nanos: i64| i128::from(secs) * 1_000_000_000 + i128::from(nanos);
        Self {
            dev: meta.dev(),
            ino: meta.ino(),
            file_type: meta.file_type(),
            mode: meta.mode() & 0o7777,
            size: meta.size(),
            mtime_ns: ns(meta.mtime(), meta.mtime_nsec()),
            ctime_ns: ns(meta.ctime(), meta.ctime_nsec()),
        }
    }

    /// A regular file (not a symlink).
    pub fn is_file(&self) -> bool {
        self.file_type.is_file()
    }

    pub fn size(&self) -> u64 {
        self.size
    }

    /// The modification time, in nanoseconds since the epoch.
    pub fn mtime_ns(&self) -> i128 {
        self.mtime_ns
    }

    pub fn dev(&self) -> u64 {
        self.dev
    }

    pub fn ino(&self) -> u64 {
        self.ino
    }

    /// The status-change time, in nanoseconds since the epoch.
    pub fn ctime_ns(&self) -> i128 {
        self.ctime_ns
    }

    /// The entry's kind, symlinks not followed.
    pub fn file_type(&self) -> FileType {
        self.file_type
    }

    /// Every field as one string: equal for equal stamps, so a preview can record what it
    /// observed and a later run compare it (M3-D46).
    pub fn fingerprint(&self) -> String {
        let kind = if self.file_type.is_file() {
            "f"
        } else if self.file_type.is_dir() {
            "d"
        } else if self.file_type.is_symlink() {
            "l"
        } else {
            "o"
        };
        format!(
            "{}:{}:{kind}:{:o}:{}:{}:{}",
            self.dev, self.ino, self.mode, self.size, self.mtime_ns, self.ctime_ns
        )
    }

    /// Whether a change made after this stamp was read at `read_at_ns` (wall clock) is
    /// sure to move its `ctime`: the `ctime` is older than the read by the racy slack,
    /// the coarse one for whole-second timestamps.
    pub fn settled(&self, read_at_ns: i128) -> bool {
        self.unsettled_ns(read_at_ns) < 0
    }

    /// Nanoseconds from `read_at_ns` until the stamp is [`settled`](Self::settled);
    /// negative once it is.
    pub fn unsettled_ns(&self, read_at_ns: i128) -> i128 {
        let slack = if self.ctime_ns.rem_euclid(1_000_000_000) == 0 {
            RACY_SLACK_COARSE_NS
        } else {
            RACY_SLACK_NS
        };
        self.ctime_ns + slack - read_at_ns
    }
}

/// Every entry of a directory tree with its stamp, by path relative to the tree (the
/// tree itself as the empty path), in byte order. Symlinks are stamped, never followed,
/// and directories count: an entry added, removed or renamed moves its directory's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeStamp(Vec<(PathBuf, FileStamp)>);

impl TreeStamp {
    pub fn read(dir: &Path) -> io::Result<Self> {
        let mut entries = vec![(PathBuf::new(), Self::root(dir)?)];
        let mut pending = vec![PathBuf::new()];
        while let Some(rel) = pending.pop() {
            for entry in fs::read_dir(dir.join(&rel))? {
                let entry = entry?;
                let rel_child = rel.join(entry.file_name());
                let stamp = FileStamp::of(&entry.metadata()?);
                if stamp.file_type.is_dir() {
                    pending.push(rel_child.clone());
                }
                entries.push((rel_child, stamp));
            }
        }
        entries.sort_by(|a, b| a.0.as_os_str().cmp(b.0.as_os_str()));
        Ok(Self(entries))
    }

    /// [`TreeStamp::read`], bounded: `None` as soon as more than `max_entries` entries
    /// (the root included) were seen. A root that is not a real directory is its one
    /// entry, never listed (a symlinked root is not followed). A directory that cannot be
    /// listed or stamped fails with a [`WalkError`] inside the `io::Error`, naming it.
    pub fn read_at_most(dir: &Path, max_entries: u64) -> io::Result<Option<Self>> {
        let root = Self::root(dir)?;
        let mut entries = vec![(PathBuf::new(), root)];
        if !root.file_type.is_dir() {
            return Ok(Some(Self(entries)));
        }
        let mut pending = vec![PathBuf::new()];
        while let Some(rel) = pending.pop() {
            let failed = |source: io::Error| WalkError::wrap(&rel, source);
            for entry in fs::read_dir(dir.join(&rel)).map_err(failed)? {
                let entry = entry.map_err(failed)?;
                let rel_child = rel.join(entry.file_name());
                let stamp = FileStamp::of(&entry.metadata().map_err(failed)?);
                if stamp.file_type.is_dir() {
                    pending.push(rel_child.clone());
                }
                entries.push((rel_child, stamp));
                if entries.len() as u64 > max_entries {
                    return Ok(None);
                }
            }
        }
        entries.sort_by(|a, b| a.0.as_os_str().cmp(b.0.as_os_str()));
        Ok(Some(Self(entries)))
    }

    fn root(dir: &Path) -> io::Result<FileStamp> {
        FileStamp::read(dir)?.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("{} does not exist", dir.display()),
            )
        })
    }

    /// Every entry with its stamp, by relative path in byte order, the root first.
    pub fn entries(&self) -> &[(PathBuf, FileStamp)] {
        &self.0
    }

    /// Whether every entry is [settled](FileStamp::settled) at `read_at_ns`.
    pub fn settled(&self, read_at_ns: i128) -> bool {
        self.0.iter().all(|(_, stamp)| stamp.settled(read_at_ns))
    }
}

/// A directory a [`TreeStamp::read_at_most`] walk could not list or stamp: its path
/// relative to the tree, and why. Carried inside the `io::Error` the walk returns.
#[derive(Debug)]
pub struct WalkError {
    pub rel: PathBuf,
    pub source: io::Error,
}

impl WalkError {
    fn wrap(rel: &Path, source: io::Error) -> io::Error {
        let kind = source.kind();
        io::Error::new(
            kind,
            Self {
                rel: rel.to_path_buf(),
                source,
            },
        )
    }

    /// The directory an `io::Error` from [`TreeStamp::read_at_most`] names, if any.
    pub fn rel_of(error: &io::Error) -> Option<&Path> {
        error
            .get_ref()
            .and_then(|inner| inner.downcast_ref::<Self>())
            .map(|walk| walk.rel.as_path())
    }
}

impl std::fmt::Display for WalkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.rel.display(), self.source)
    }
}

impl std::error::Error for WalkError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::time::Duration;

    /// Longer than the kernel's file-timestamp tick, so the next change gets a new
    /// `ctime`.
    fn tick() {
        std::thread::sleep(Duration::from_millis(30));
    }

    fn stamp(path: &Path) -> FileStamp {
        FileStamp::read(path).unwrap().unwrap()
    }

    #[test]
    fn a_file_stamp_moves_with_every_change() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Foo.lua");
        fs::write(&path, "-- one\n").unwrap();
        let first = stamp(&path);
        assert!(first.is_file());
        assert_eq!(stamp(&path), first, "nothing changed");
        assert_eq!(FileStamp::read(&dir.path().join("absent")).unwrap(), None);

        // Rewritten in place, same size, its mtime put back: ctime still moves.
        tick();
        let mtime = fs::metadata(&path).unwrap().modified().unwrap();
        fs::write(&path, "-- two\n").unwrap();
        fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(mtime)
            .unwrap();
        let rewritten = stamp(&path);
        assert_eq!(rewritten.size(), first.size());
        assert_eq!(rewritten.mtime_ns, first.mtime_ns, "mtime preserved");
        assert_ne!(rewritten, first, "ctime moved");

        // Replaced by a rename: another inode.
        tick();
        let other = dir.path().join("other");
        fs::write(&other, "-- two\n").unwrap();
        fs::rename(&other, &path).unwrap();
        assert_ne!(stamp(&path), rewritten);

        // A mode change.
        tick();
        let before = stamp(&path);
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert_ne!(stamp(&path), before);

        // A symlink is stamped, not followed.
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert!(!stamp(&link).is_file());
    }

    #[test]
    fn a_stamp_settles_after_the_racy_slack() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Foo.lua");
        fs::write(&path, "x").unwrap();
        let s = stamp(&path);
        let fine = |ctime_ns| FileStamp { ctime_ns, ..s };
        let ctime = 1_700_000_000_123_456_789;
        assert!(!fine(ctime).settled(ctime), "read in the same tick");
        assert!(!fine(ctime).settled(ctime + RACY_SLACK_NS));
        assert!(fine(ctime).settled(ctime + RACY_SLACK_NS + 1));
        assert_eq!(fine(ctime).unsettled_ns(ctime), RACY_SLACK_NS);
        let coarse = 1_700_000_000_000_000_000;
        assert!(
            !fine(coarse).settled(coarse + RACY_SLACK_NS + 1),
            "whole-second timestamps take the coarse slack"
        );
        assert!(fine(coarse).settled(coarse + RACY_SLACK_COARSE_NS + 1));
    }

    #[test]
    fn a_tree_stamp_sees_adds_removes_renames_and_modes() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("Foo");
        fs::create_dir_all(root.join("libs")).unwrap();
        fs::write(root.join("Foo.toc"), "## Interface: 120105\n").unwrap();
        fs::write(root.join("libs/lib.lua"), "-- lib\n").unwrap();
        let read = || TreeStamp::read(&root).unwrap();
        let first = read();
        assert_eq!(read(), first, "nothing changed");
        let paths: Vec<&Path> = first.0.iter().map(|(p, _)| p.as_path()).collect();
        assert_eq!(
            paths,
            [
                Path::new(""),
                Path::new("Foo.toc"),
                Path::new("libs"),
                Path::new("libs/lib.lua")
            ]
        );

        let mut last = first;
        let mut changed = |what: &str, change: &dyn Fn()| {
            tick();
            change();
            let now = read();
            assert_ne!(now, last, "{what}");
            last = now;
        };
        changed("an added file", &|| {
            fs::write(root.join("libs/new.lua"), "").unwrap();
        });
        changed("a removed file", &|| {
            fs::remove_file(root.join("libs/new.lua")).unwrap();
        });
        changed("a renamed file", &|| {
            fs::rename(root.join("libs/lib.lua"), root.join("libs/Lib.lua")).unwrap();
        });
        changed("a mode change", &|| {
            fs::set_permissions(root.join("Foo.toc"), fs::Permissions::from_mode(0o600)).unwrap();
        });
        changed("a content change deep inside", &|| {
            fs::write(root.join("libs/Lib.lua"), "-- LIB\n").unwrap();
        });
        assert!(TreeStamp::read(&dir.path().join("absent")).is_err());
    }

    #[test]
    fn read_at_most_stops_past_the_bound() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("Foo");
        fs::create_dir_all(root.join("libs")).unwrap();
        fs::write(root.join("Foo.toc"), "## Interface: 120105\n").unwrap();
        fs::write(root.join("libs/lib.lua"), "-- lib\n").unwrap();
        assert_eq!(
            TreeStamp::read_at_most(&root, 3).unwrap(),
            None,
            "four entries"
        );
        assert_eq!(
            TreeStamp::read_at_most(&root, 4).unwrap(),
            Some(TreeStamp::read(&root).unwrap())
        );
        // A symlinked root is its one entry, never listed.
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&root, &link).unwrap();
        let linked = TreeStamp::read_at_most(&link, 10).unwrap().unwrap();
        assert_eq!(linked.entries().len(), 1);
        assert!(linked.entries()[0].1.file_type().is_symlink());
    }

    #[test]
    fn the_accessors_report_the_stamp() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("Foo.lua");
        fs::write(&file, "x").unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&file, &link).unwrap();
        for (path, kind) in [
            (&file, "file"),
            (&dir.path().to_path_buf(), "dir"),
            (&link, "symlink"),
        ] {
            let meta = fs::symlink_metadata(path).unwrap();
            let s = stamp(path);
            assert_eq!((s.dev(), s.ino()), (meta.dev(), meta.ino()), "{kind}");
            let ctime = i128::from(meta.ctime()) * 1_000_000_000 + i128::from(meta.ctime_nsec());
            assert_eq!(s.ctime_ns(), ctime, "{kind}");
            assert_eq!(s.file_type(), meta.file_type(), "{kind}");
        }
    }

    #[test]
    fn a_walk_error_names_the_directory_it_could_not_list() {
        if rustix::process::geteuid().is_root() {
            return; // root lists a mode-000 directory
        }
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("Foo");
        fs::create_dir_all(root.join("locked")).unwrap();
        fs::set_permissions(root.join("locked"), fs::Permissions::from_mode(0o000)).unwrap();
        let error = TreeStamp::read_at_most(&root, 10).unwrap_err();
        fs::set_permissions(root.join("locked"), fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            WalkError::rel_of(&error),
            Some(Path::new("locked")),
            "{error}"
        );
    }
}
