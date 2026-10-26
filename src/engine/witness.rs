//! The approval witness's foundation (M4-D60): the content of one folder as an
//! [`EntryDigest`], in which every entry kind counts (a symlink, an empty directory and
//! an empty file are content too), and the folder's stat stamp as private proof of
//! "unchanged since" once the folder has settled. A file whose stamp proves it
//! racy-clean against the files index takes its digest from the index, as drift's stat
//! mode trusts it (plus a ctime test, which an mtime put back cannot fool); every other
//! file is hashed.
//!
//! Read-only, and careful about what it opens: only entries the stamp calls regular
//! files, never following a link and never blocking, each checked by inode once open. A
//! folder that cannot be read completely fails closed: a token must never approve bytes
//! nobody observed.

use std::fs::FileType;
use std::io::{self, Read};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::FileTypeExt;
use std::path::{Path, PathBuf};

use rustix::fs::{Mode, OFlags};
use rustix::io::Errno;
use sha2::{Digest, Sha256};

use super::inspect::ArchiveLimits;
use crate::error::{Error, Result, Suggestion};
use crate::fsx::{FileStamp, TreeStamp, WalkError};
use crate::state::files_index::{
    FilesIndex, FolderIndex, hash_reader, limit_exceeded, unindexable,
};

/// The content of one folder, every entry kind counted: a SHA-256 over length-prefixed
/// fields, each entry by its raw relative path in byte order. A `touch` or a `chmod`
/// does not move it; an added empty directory, empty file or symlink does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EntryDigest([u8; 32]);

impl std::fmt::Display for EntryDigest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("entry:sha256:")?;
        self.0.iter().try_for_each(|b| write!(f, "{b:02x}"))
    }
}

impl EntryDigest {
    /// The digest of the folder at `dir` with every file hashed (no index): the re-hash
    /// under the commit lock.
    pub fn of_dir(dir: &Path, limits: &ArchiveLimits) -> Result<Self> {
        folder(dir, None, limits, i128::MAX, &mut || {}).map(|f| f.digest)
    }
}

/// What a witnessed folder is to the plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum WitnessRole {
    Replaced,
    Removed,
    CopySource,
    Adopted,
}

impl WitnessRole {
    pub const fn wire(self) -> &'static str {
        match self {
            Self::Replaced => "replaced",
            Self::Removed => "removed",
            Self::CopySource => "copy_source",
            Self::Adopted => "adopted",
        }
    }
}

/// One folder of a target's witness: its role, its key and its content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderWitness {
    pub role: WitnessRole,
    pub key: String,
    pub digest: EntryDigest,
}

/// What witnessing cost: files hashed, files whose digest the index gave, bytes hashed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WitnessCost {
    pub files_hashed: u64,
    pub files_reused: u64,
    pub bytes_hashed: u64,
}

/// One folder's evidence: its digest, its stamp when it was settled and did not move
/// while read (the under-lock check compares it; without one, the folder is hashed
/// again), and the cost.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Folder {
    pub digest: EntryDigest,
    pub stamp: Option<TreeStamp>,
    pub cost: WitnessCost,
}

/// One entry as the digest encodes it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Entry {
    Dir,
    File {
        size: u64,
        sha256: String,
    },
    Link(Vec<u8>),
    /// A FIFO, socket, device or unknown kind, never opened.
    Other(&'static str),
    /// It changed while it was read: matches no steady-state digest.
    Changed,
    /// The folder itself is absent.
    Absent,
}

impl Entry {
    const fn kind(&self) -> &'static str {
        match self {
            Self::Dir => "d",
            Self::File { .. } => "f",
            Self::Link(_) => "l",
            Self::Other(kind) => kind,
            Self::Changed => "x",
            Self::Absent => "a",
        }
    }
}

/// A big-endian `u64` length, then the bytes: no name can forge a boundary.
fn field(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update((bytes.len() as u64).to_be_bytes());
    hasher.update(bytes);
}

fn digest_of(entries: &[(Vec<u8>, Entry)]) -> EntryDigest {
    let mut hasher = Sha256::new();
    field(&mut hasher, b"omaddons entry digest 1");
    for (path, entry) in entries {
        field(&mut hasher, entry.kind().as_bytes());
        field(&mut hasher, path);
        match entry {
            Entry::File { size, sha256 } => {
                field(&mut hasher, size.to_string().as_bytes());
                field(&mut hasher, sha256.as_bytes());
            }
            Entry::Link(target) => field(&mut hasher, target),
            Entry::Dir | Entry::Other(_) | Entry::Changed | Entry::Absent => {}
        }
    }
    EntryDigest(hasher.finalize().into())
}

/// The evidence of the folder at `dir` (see the module doc). `index` is the folder's
/// files index entry with its index, when the folder is managed; `now_ns` is the wall
/// clock taken before the first read (settledness is judged against it); `between` runs
/// after the entries are digested and before the stamp is read again (a no-op outside
/// tests).
pub fn folder(
    dir: &Path,
    index: Option<(&FilesIndex, &FolderIndex)>,
    limits: &ArchiveLimits,
    now_ns: i128,
    between: &mut dyn FnMut(),
) -> Result<Folder> {
    let name = dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut cost = WitnessCost::default();
    let mut retried = false;
    loop {
        let root = FileStamp::read(dir).map_err(|e| unreadable(&name, Path::new(""), &e))?;
        let Some(root) = root else {
            let digest = digest_of(&[(Vec::new(), Entry::Absent)]);
            return Ok(Folder {
                digest,
                stamp: None,
                cost,
            });
        };
        if !root.file_type().is_dir() {
            // A symlink, file or other kind at the folder's place: one entry, never walked.
            let entry = encode(dir, Path::new(""), &root, None, &name, limits, &mut cost)?;
            return Ok(Folder {
                digest: digest_of(&[(Vec::new(), entry)]),
                stamp: None,
                cost,
            });
        }
        let first = match TreeStamp::read_at_most(dir, limits.max_entries) {
            Ok(Some(stamp)) => stamp,
            Ok(None) => {
                return Err(limit_exceeded(
                    &name,
                    "entries",
                    limits.max_entries.saturating_add(1),
                    limits.max_entries,
                    "limits.archive_max_entries",
                ));
            }
            // Something moved while it was listed: read it once more.
            Err(e) if e.kind() == io::ErrorKind::NotFound && !retried => {
                retried = true;
                continue;
            }
            Err(e) => {
                let rel = WalkError::rel_of(&e).unwrap_or_else(|| Path::new(""));
                return Err(unreadable(&name, rel, &e));
            }
        };
        // Replaced between the two reads: start again once, then take it as it is.
        let root_is_dir = first
            .entries()
            .first()
            .is_some_and(|(_, stamp)| stamp.file_type().is_dir());
        if !root_is_dir && !retried {
            retried = true;
            continue;
        }
        let mut entries = Vec::with_capacity(first.entries().len());
        for (rel, stamp) in first.entries() {
            let entry = encode(dir, rel, stamp, index, &name, limits, &mut cost)?;
            entries.push((rel.as_os_str().as_bytes().to_vec(), entry));
        }
        between();
        let again = TreeStamp::read_at_most(dir, limits.max_entries)
            .ok()
            .flatten();
        let steady = again.as_ref() == Some(&first) && first.settled(now_ns);
        return Ok(Folder {
            digest: digest_of(&entries),
            stamp: steady.then_some(first),
            cost,
        });
    }
}

/// `dir/rel`, the folder itself for the empty path (no trailing slash).
fn full(dir: &Path, rel: &Path) -> PathBuf {
    if rel.as_os_str().is_empty() {
        dir.to_path_buf()
    } else {
        dir.join(rel)
    }
}

/// One entry, encoded from its stamp; a regular file's digest from the index when it is
/// racy-clean there, else hashed.
fn encode(
    dir: &Path,
    rel: &Path,
    stamp: &FileStamp,
    index: Option<(&FilesIndex, &FolderIndex)>,
    name: &str,
    limits: &ArchiveLimits,
    cost: &mut WitnessCost,
) -> Result<Entry> {
    let path = full(dir, rel);
    let kind = stamp.file_type();
    if kind.is_dir() {
        return Ok(Entry::Dir);
    }
    if kind.is_symlink() {
        return match std::fs::read_link(&path) {
            Ok(target) => Ok(Entry::Link(target.into_os_string().into_vec())),
            // Gone, or no longer a symlink.
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::InvalidInput
                ) =>
            {
                Ok(Entry::Changed)
            }
            Err(e) => Err(unreadable(name, rel, &e)),
        };
    }
    if !kind.is_file() {
        return Ok(Entry::Other(other_kind(kind)));
    }
    if let Some(reused) = reuse(rel, stamp, index) {
        cost.files_reused += 1;
        return Ok(reused);
    }
    let over = |measured: u64| {
        limit_exceeded(
            name,
            "bytes",
            measured,
            limits.max_uncompressed,
            "limits.archive_max_uncompressed",
        )
    };
    let remaining = limits.max_uncompressed.saturating_sub(cost.bytes_hashed);
    if stamp.size() > remaining {
        return Err(over(cost.bytes_hashed.saturating_add(stamp.size())));
    }
    let Some(file) = open_regular(&path, stamp).map_err(|e| unreadable(name, rel, &e))? else {
        return Ok(Entry::Changed);
    };
    // Bounded even for a file that grows while it is read.
    let (size, sha256) = hash_reader(file.take(remaining.saturating_add(1)))
        .map_err(|e| unreadable(name, rel, &e))?;
    cost.files_hashed += 1;
    cost.bytes_hashed = cost.bytes_hashed.saturating_add(size);
    if size > remaining {
        return Err(over(cost.bytes_hashed));
    }
    Ok(Entry::File { size, sha256 })
}

/// The index's digest of the file at `rel`, when its stamp proves it racy-clean: the
/// size and mtime the index recorded, and an mtime and a ctime both strictly older than
/// the moment the entries were recorded less drift's slack. User space can put an mtime
/// back, never a ctime.
fn reuse(
    rel: &Path,
    stamp: &FileStamp,
    index: Option<(&FilesIndex, &FolderIndex)>,
) -> Option<Entry> {
    let (files, folder) = index?;
    let rel = rel.to_str()?;
    let at = folder
        .files
        .binary_search_by(|f| f.path.as_bytes().cmp(rel.as_bytes()))
        .ok()?;
    let entry = folder.files.get(at)?;
    let racy_from = files.recorded_at_ns(folder) - crate::drift::slack(folder);
    let clean = stamp.size() == entry.size
        && stamp.mtime_ns() == entry.mtime_ns
        && stamp.mtime_ns() < racy_from
        && stamp.ctime_ns() < racy_from;
    clean.then(|| Entry::File {
        size: entry.size,
        sha256: entry.sha256.clone(),
    })
}

/// The regular file the stamp describes, opened read-only without following a link or
/// blocking, and checked to be that inode; `None` when it is gone, a symlink, not a
/// regular file or another inode (it changed).
fn open_regular(path: &Path, stamp: &FileStamp) -> io::Result<Option<std::fs::File>> {
    let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
    let fd = match rustix::fs::open(path, flags, Mode::empty()) {
        Ok(fd) => fd,
        Err(e) if [Errno::NOENT, Errno::LOOP, Errno::NXIO, Errno::NOTDIR].contains(&e) => {
            return Ok(None);
        }
        Err(e) => return Err(e.into()),
    };
    let stat = rustix::fs::fstat(&fd)?;
    let regular = rustix::fs::FileType::from_raw_mode(stat.st_mode).is_file();
    let same = stat.st_dev == stamp.dev() && stat.st_ino == stamp.ino();
    Ok((regular && same).then(|| std::fs::File::from(fd)))
}

fn other_kind(kind: FileType) -> &'static str {
    if kind.is_fifo() {
        "o:fifo"
    } else if kind.is_socket() {
        "o:socket"
    } else if kind.is_char_device() {
        "o:char"
    } else if kind.is_block_device() {
        "o:block"
    } else {
        "o:unknown"
    }
}

/// `unindexable` for an entry the witness cannot read (the folder itself as `.`).
fn unreadable(name: &str, rel: &Path, e: &io::Error) -> Error {
    let rel = if rel.as_os_str().is_empty() {
        ".".to_owned()
    } else {
        rel.to_string_lossy().into_owned()
    };
    unindexable(
        name,
        &rel,
        format!(
            "{name}/{rel} cannot be read ({e}), so the folder cannot be witnessed for this preview"
        ),
        Suggestion::text("make it readable, or move the folder out of AddOns, then preview again"),
    )
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::ffi::OsStr;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::time::Duration;

    use super::*;
    use crate::drift::{DriftMode, check_folder};
    use crate::error::ErrorCode;
    use crate::model::{FolderName, TargetKey};
    use crate::state::files_index::index_live_folder;

    /// Longer than the kernel's file-timestamp tick, so the next change gets a new ctime.
    fn tick() {
        std::thread::sleep(Duration::from_millis(30));
    }

    /// A folder `Foo` holding `files` (`/`-separated paths), in a fresh tempdir.
    fn folder_with(files: &[(&str, &[u8])]) -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("Foo");
        fs::create_dir(&dir).unwrap();
        for (path, bytes) in files {
            let path = dir.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, bytes).unwrap();
        }
        (tmp, dir)
    }

    /// The index of `dir` as a commit would record it, recorded at `at_ns`.
    fn indexed(dir: &Path, at_ns: i128) -> (FilesIndex, FolderIndex) {
        let folder = index_live_folder(dir, &ArchiveLimits::default())
            .unwrap()
            .stamped(at_ns);
        let files = FilesIndex {
            key: TargetKey::new(),
            written_at_ns: 0,
            folders: BTreeMap::from([("foo".to_owned(), folder.clone())]),
        };
        (files, folder)
    }

    /// The newest of every entry's mtime and ctime.
    fn newest(dir: &Path) -> i128 {
        TreeStamp::read(dir)
            .unwrap()
            .entries()
            .iter()
            .map(|(_, s)| s.mtime_ns().max(s.ctime_ns()))
            .max()
            .unwrap()
    }

    fn of_dir(dir: &Path) -> EntryDigest {
        EntryDigest::of_dir(dir, &ArchiveLimits::default()).unwrap()
    }

    fn run(dir: &Path, index: Option<(&FilesIndex, &FolderIndex)>) -> Folder {
        folder(dir, index, &ArchiveLimits::default(), i128::MAX, &mut || {}).unwrap()
    }

    /// Every entry's kind and path, as the digest encodes them.
    fn kinds(dir: &Path) -> Vec<(String, Vec<u8>)> {
        let stamp = TreeStamp::read_at_most(dir, u64::MAX).unwrap().unwrap();
        let mut cost = WitnessCost::default();
        stamp
            .entries()
            .iter()
            .map(|(rel, s)| {
                let limits = ArchiveLimits::default();
                let entry = encode(dir, rel, s, None, "Foo", &limits, &mut cost).unwrap();
                (entry.kind().to_owned(), rel.as_os_str().as_bytes().to_vec())
            })
            .collect()
    }

    #[test]
    fn the_entry_digest_counts_every_kind() {
        let base: &[(&str, &[u8])] = &[
            ("Foo.toc", b"## Title: Foo\n"),
            ("libs/lib.lua", b"-- lib\n"),
        ];
        type Change = fn(&Path);
        let rows: &[(&str, Change, bool)] = &[
            (
                "added empty dir",
                |d| fs::create_dir(d.join("empty")).unwrap(),
                true,
            ),
            (
                "added empty file",
                |d| fs::write(d.join("empty.lua"), "").unwrap(),
                true,
            ),
            (
                "added symlink",
                |d| std::os::unix::fs::symlink("Foo.toc", d.join("link")).unwrap(),
                true,
            ),
            (
                "symlink retargeted",
                |d| {
                    fs::remove_file(d.join("link")).unwrap();
                    std::os::unix::fs::symlink("libs", d.join("link")).unwrap();
                },
                true,
            ),
            (
                "file renamed",
                |d| fs::rename(d.join("libs/lib.lua"), d.join("libs/Lib.lua")).unwrap(),
                true,
            ),
            (
                "file deleted",
                |d| fs::remove_file(d.join("Foo.toc")).unwrap(),
                true,
            ),
            (
                "same size, one byte changed",
                |d| fs::write(d.join("libs/lib.lua"), "-- LIB\n").unwrap(),
                true,
            ),
            (
                "file grew",
                |d| fs::write(d.join("libs/lib.lua"), "-- lib\n-- more\n").unwrap(),
                true,
            ),
            (
                "touch",
                |d| {
                    let file = fs::File::options()
                        .write(true)
                        .open(d.join("Foo.toc"))
                        .unwrap();
                    file.set_modified(std::time::SystemTime::now()).unwrap();
                },
                false,
            ),
            (
                "chmod 600",
                |d| {
                    fs::set_permissions(d.join("Foo.toc"), fs::Permissions::from_mode(0o600))
                        .unwrap()
                },
                false,
            ),
            (
                "recreated identical (new inode)",
                |d| {
                    let bytes = fs::read(d.join("Foo.toc")).unwrap();
                    fs::remove_file(d.join("Foo.toc")).unwrap();
                    fs::write(d.join("Foo.toc"), bytes).unwrap();
                },
                false,
            ),
        ];
        for (row, change, moves) in rows {
            // Each row on a fresh base, plus the symlink the retarget row needs.
            let (_tmp, dir) = folder_with(base);
            if *row == "symlink retargeted" {
                std::os::unix::fs::symlink("Foo.toc", dir.join("link")).unwrap();
            }
            let before = run(&dir, None);
            tick();
            change(&dir);
            let after = run(&dir, None);
            // ubs:ignore — comparing content digests, not secrets
            let moved = before.digest != after.digest;
            assert_eq!(
                moved, *moves,
                "{row}: digest {} vs {}, cost {:?}",
                before.digest, after.digest, after.cost
            );
        }
    }

    #[test]
    fn the_eleventh_changed_file_moves_the_digest() {
        let names: Vec<String> = (0..11).map(|i| format!("f{i:02}.lua")).collect();
        let files: Vec<(&str, &[u8])> =
            names.iter().map(|n| (n.as_str(), &b"-- a\n"[..])).collect();
        let (_tmp, dir) = folder_with(&files);
        for name in &names[..10] {
            fs::write(dir.join(name), "-- b\n").unwrap();
        }
        let ten = of_dir(&dir);
        fs::write(dir.join(&names[10]), "-- b\n").unwrap();
        let eleven = of_dir(&dir);
        assert_ne!(ten, eleven, "digest {ten} vs {eleven}");
    }

    #[test]
    fn a_root_that_is_not_a_directory_is_one_entry() {
        let (tmp, dir) = folder_with(&[("Foo.toc", b"x")]);
        let real = of_dir(&dir);
        // A copy elsewhere, reached through a symlink at the folder's place.
        let copy = tmp.path().join("Copy");
        fs::create_dir(&copy).unwrap();
        fs::write(copy.join("Foo.toc"), "x").unwrap();
        let link = tmp.path().join("Linked");
        std::os::unix::fs::symlink(&copy, &link).unwrap();
        let linked = run(&link, None);
        assert_ne!(
            linked.digest, real,
            "a symlinked root is not its target's content"
        );
        assert_eq!(linked.cost.files_hashed, 0, "never walked");
        assert_eq!(kinds(&link), [("l".to_owned(), Vec::new())]);
        let file = tmp.path().join("File");
        fs::write(&file, "x").unwrap();
        assert_eq!(kinds(&file), [("f".to_owned(), Vec::new())]);
        let absent = run(&tmp.path().join("Absent"), None);
        assert_eq!(absent.digest, digest_of(&[(Vec::new(), Entry::Absent)]));
        assert_eq!(absent.stamp, None);
    }

    #[test]
    fn clean_indexed_files_are_reused_and_others_hashed() {
        let (_tmp, dir) = folder_with(&[
            ("Foo.toc", b"## Title: Foo\n"),
            ("a.lua", b"a"),
            ("b/c.lua", b"c"),
        ]);
        let (files, folder) = indexed(&dir, newest(&dir) + 1_000_000_000);
        let clean = run(&dir, Some((&files, &folder)));
        assert_eq!(
            (clean.cost.files_reused, clean.cost.files_hashed),
            (3, 0),
            "cost {:?}",
            clean.cost
        );
        assert_eq!(
            clean.digest,
            of_dir(&dir),
            "the reused digest is the hashed one"
        );
        tick();
        fs::write(dir.join("a.lua"), "A").unwrap();
        let edited = run(&dir, Some((&files, &folder)));
        assert_eq!(edited.cost.files_hashed, 1, "cost {:?}", edited.cost);
        assert_eq!(edited.digest, of_dir(&dir));
    }

    #[test]
    fn the_racy_clean_edge() {
        let (_tmp, dir) = folder_with(&[("Foo.lua", b"-- foo\n")]);
        let at = newest(&dir);
        let slack = crate::limits::RACY_SLACK_NS;
        let hashed = |files: &FilesIndex, folder: &FolderIndex| {
            run(&dir, Some((files, folder))).cost.files_hashed
        };
        let (files, folder) = indexed(&dir, at + slack + 1);
        assert_eq!(
            hashed(&files, &folder),
            0,
            "recorded at ctime + slack + 1: reused"
        );
        let (files, folder) = indexed(&dir, at + slack);
        assert_eq!(
            hashed(&files, &folder),
            1,
            "recorded at ctime + slack: hashed"
        );
        let (files, mut folder) = indexed(&dir, at + slack + 1);
        folder.files[0].mtime_ns += 1;
        assert_eq!(hashed(&files, &folder), 1, "index mtime differs: hashed");
        let (files, mut folder) = indexed(&dir, at + slack + 1);
        folder.files[0].size += 1;
        assert_eq!(hashed(&files, &folder), 1, "index size differs: hashed");
        // Whole-second index mtimes take the coarse slack.
        let whole = std::time::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let file = fs::File::options()
            .write(true)
            .open(dir.join("Foo.lua"))
            .unwrap();
        file.set_modified(whole).unwrap();
        let at = newest(&dir);
        let (files, folder) = indexed(&dir, at + slack + 1);
        assert_eq!(folder.files[0].mtime_ns.rem_euclid(1_000_000_000), 0);
        assert_eq!(
            hashed(&files, &folder),
            1,
            "whole-second mtimes: the coarse slack applies"
        );
        let coarse = crate::limits::RACY_SLACK_COARSE_NS;
        let (files, folder) = indexed(&dir, at + coarse + 1);
        assert_eq!(hashed(&files, &folder), 0, "past the coarse slack: reused");
    }

    #[test]
    fn an_mtime_preserving_same_size_edit_is_hashed() {
        let (tmp, dir) = folder_with(&[("Foo.lua", b"-- one\n")]);
        let (files, folder) = indexed(&dir, newest(&dir) + crate::limits::RACY_SLACK_NS + 1);
        let before = run(&dir, Some((&files, &folder)));
        assert_eq!(before.cost.files_reused, 1);
        tick();
        let mtime = fs::metadata(dir.join("Foo.lua"))
            .unwrap()
            .modified()
            .unwrap();
        fs::write(dir.join("Foo.lua"), "-- two\n").unwrap();
        let file = fs::File::options()
            .write(true)
            .open(dir.join("Foo.lua"))
            .unwrap();
        file.set_modified(mtime).unwrap();
        let after = run(&dir, Some((&files, &folder)));
        assert_eq!(after.cost.files_hashed, 1, "cost {:?}", after.cost);
        assert_ne!(
            after.digest, before.digest,
            "digest {} vs {}",
            after.digest, before.digest
        );
        // The control: drift's stat mode, which reads no ctime, sees nothing.
        let name = FolderName::new("Foo").unwrap();
        let drift = check_folder(
            tmp.path(),
            &name,
            Some(&folder),
            files.recorded_at_ns(&folder),
            DriftMode::Stat,
        );
        assert_eq!(drift.drift.changed, 0, "{:?}", drift.drift);
    }

    #[test]
    fn a_trusted_index_digest_is_used_as_recorded() {
        let (_tmp, dir) = folder_with(&[("Foo.lua", b"-- foo\n")]);
        let (files, mut folder) = indexed(&dir, newest(&dir) + 1_000_000_000);
        folder.files[0].sha256 = "0".repeat(64);
        let trusted = run(&dir, Some((&files, &folder)));
        assert_eq!(trusted.cost.files_reused, 1);
        assert_ne!(
            trusted.digest,
            of_dir(&dir),
            "the index is believed, as drift believes it"
        );
    }

    #[test]
    fn a_folder_that_moves_while_hashed_keeps_no_stamp() {
        let (_tmp, dir) = folder_with(&[("Foo.lua", b"-- foo\n")]);
        let before = of_dir(&dir);
        let mut edit = || fs::write(dir.join("new.lua"), "-- new\n").unwrap();
        let moved = folder(&dir, None, &ArchiveLimits::default(), i128::MAX, &mut edit).unwrap();
        assert_eq!(moved.stamp, None);
        assert_eq!(moved.digest, before, "the digest is the content read");
    }

    #[test]
    fn an_unsettled_folder_keeps_no_stamp() {
        let (_tmp, dir) = folder_with(&[("Foo.lua", b"-- foo\n"), ("libs/lib.lua", b"-- lib\n")]);
        let ctime = TreeStamp::read(&dir)
            .unwrap()
            .entries()
            .iter()
            .map(|(_, s)| s.ctime_ns())
            .max()
            .unwrap();
        let at = |now| folder(&dir, None, &ArchiveLimits::default(), now, &mut || {}).unwrap();
        assert_eq!(
            at(ctime + crate::limits::RACY_SLACK_NS).stamp,
            None,
            "unsettled"
        );
        assert_eq!(
            at(ctime + crate::limits::RACY_SLACK_NS + 1).stamp,
            Some(TreeStamp::read(&dir).unwrap()),
            "settled"
        );
    }

    #[test]
    fn a_fifo_is_recorded_and_never_opened() {
        let (_tmp, dir) = folder_with(&[("Foo.lua", b"-- foo\n")]);
        rustix::fs::mknodat(
            rustix::fs::CWD,
            dir.join("pipe"),
            rustix::fs::FileType::Fifo,
            Mode::from_raw_mode(0o644),
            0,
        )
        .unwrap();
        let (send, receive) = std::sync::mpsc::channel();
        let walked = dir.clone();
        std::thread::spawn(move || {
            let _ = send.send(of_dir(&walked));
        });
        assert!(
            receive.recv_timeout(Duration::from_secs(5)).is_ok(),
            "the FIFO was opened"
        );
        assert!(
            kinds(&dir).contains(&("o:fifo".to_owned(), b"pipe".to_vec())),
            "{:?}",
            kinds(&dir)
        );
    }

    #[test]
    fn a_symlink_is_never_followed() {
        let (_tmp, dir) = folder_with(&[("Foo.lua", b"-- foo\n")]);
        std::os::unix::fs::symlink("/", dir.join("escape")).unwrap();
        std::os::unix::fs::symlink(".", dir.join("loop")).unwrap();
        let walked = run(&dir, None);
        assert_eq!(
            walked.cost.files_hashed, 1,
            "only Foo.lua: cost {:?}",
            walked.cost
        );
        let found = kinds(&dir);
        assert!(
            found.contains(&("l".to_owned(), b"escape".to_vec())),
            "{found:?}"
        );
        assert!(
            found.contains(&("l".to_owned(), b"loop".to_vec())),
            "{found:?}"
        );
    }

    #[test]
    fn open_regular_refuses_another_inode_and_never_blocks() {
        let (_tmp, dir) = folder_with(&[("Foo.lua", b"-- foo\n"), ("Bar.lua", b"-- bar\n")]);
        let stamp_of = |name: &str| FileStamp::read(&dir.join(name)).unwrap().unwrap();
        let foo = stamp_of("Foo.lua");
        rustix::fs::mknodat(
            rustix::fs::CWD,
            dir.join("pipe"),
            rustix::fs::FileType::Fifo,
            Mode::from_raw_mode(0o644),
            0,
        )
        .unwrap();
        std::os::unix::fs::symlink("Foo.lua", dir.join("link")).unwrap();
        for (case, path) in [
            ("a FIFO", dir.join("pipe")),
            ("a symlink", dir.join("link")),
            ("a gone path", dir.join("gone.lua")),
            ("another inode", dir.join("Bar.lua")),
        ] {
            let opened = open_regular(&path, &foo).unwrap();
            assert!(opened.is_none(), "{case}");
        }
        assert!(
            open_regular(&dir.join("Foo.lua"), &foo).unwrap().is_some(),
            "the file itself"
        );
    }

    #[test]
    fn a_non_utf8_name_is_hashed_by_its_bytes() {
        let digest = |name: &[u8]| {
            let (_tmp, dir) = folder_with(&[]);
            fs::write(dir.join(OsStr::from_bytes(name)), "x").unwrap();
            of_dir(&dir)
        };
        assert_ne!(digest(b"\xff"), digest(b"\xfe"));
    }

    fn assert_unindexable(error: &Error, path: &str) {
        assert_eq!(error.code, ErrorCode::Io, "{}", error.message);
        assert_eq!(
            error.details,
            serde_json::json!({ "reason": "unindexable", "folder": "Foo", "path": path }),
            "{}",
            error.message
        );
    }

    #[test]
    fn an_unreadable_file_fails_with_unindexable() {
        if rustix::process::geteuid().is_root() {
            return; // root reads a mode-000 file
        }
        let (_tmp, dir) = folder_with(&[("Foo.lua", b"-- foo\n")]);
        fs::set_permissions(dir.join("Foo.lua"), fs::Permissions::from_mode(0o000)).unwrap();
        let error = EntryDigest::of_dir(&dir, &ArchiveLimits::default()).unwrap_err();
        fs::set_permissions(dir.join("Foo.lua"), fs::Permissions::from_mode(0o644)).unwrap();
        assert_unindexable(&error, "Foo.lua");
    }

    #[test]
    fn an_unreadable_directory_fails_with_unindexable() {
        if rustix::process::geteuid().is_root() {
            return; // root lists a mode-000 directory
        }
        let (_tmp, dir) = folder_with(&[("libs/lib.lua", b"-- lib\n")]);
        fs::set_permissions(dir.join("libs"), fs::Permissions::from_mode(0o000)).unwrap();
        let error = EntryDigest::of_dir(&dir, &ArchiveLimits::default()).unwrap_err();
        fs::set_permissions(dir.join("libs"), fs::Permissions::from_mode(0o755)).unwrap();
        assert_unindexable(&error, "libs");
    }

    #[test]
    fn past_the_entry_limit_fails_with_limit_exceeded() {
        let (_tmp, dir) = folder_with(&[("a", b"a"), ("b", b"b"), ("c/d", b"d")]);
        let limits = ArchiveLimits {
            max_entries: 3,
            ..ArchiveLimits::default()
        };
        let error = EntryDigest::of_dir(&dir, &limits).unwrap_err();
        assert_eq!(error.code, ErrorCode::ArchiveTooLarge, "{}", error.message);
        assert_eq!(
            error.details,
            serde_json::json!({
                "reason": "limit_exceeded",
                "folder": "Foo",
                "measured": 4,
                "limit": 3,
                "config_key": "limits.archive_max_entries",
            })
        );
    }

    #[test]
    fn past_the_byte_limit_fails_but_reused_bytes_do_not_count() {
        let (_tmp, dir) = folder_with(&[("Foo.lua", &[b'x'; 20])]);
        let limits = ArchiveLimits {
            max_uncompressed: 10,
            ..ArchiveLimits::default()
        };
        let error = folder(&dir, None, &limits, i128::MAX, &mut || {}).unwrap_err();
        assert_eq!(error.code, ErrorCode::ArchiveTooLarge, "{}", error.message);
        assert_eq!(error.details["measured"], 20);
        assert_eq!(
            error.details["config_key"],
            "limits.archive_max_uncompressed"
        );
        let (files, folder_index) = indexed(&dir, newest(&dir) + 1_000_000_000);
        let reused = folder(
            &dir,
            Some((&files, &folder_index)),
            &limits,
            i128::MAX,
            &mut || {},
        );
        assert!(
            reused.is_ok(),
            "reused bytes count against no bound: {reused:?}"
        );
    }

    #[test]
    fn entry_digest_displays_as_entry_sha256() {
        let (_tmp, dir) = folder_with(&[("Foo.lua", b"x")]);
        let shown = of_dir(&dir).to_string();
        let hex = shown.strip_prefix("entry:sha256:").unwrap_or_default();
        assert_eq!(hex.len(), 64, "{shown}");
        assert!(
            hex.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "{shown}"
        );
        assert_eq!(WitnessRole::CopySource.wire(), "copy_source");
    }
}
