//! Endpoint predicates (M4-D31, M4-D60): what must be at a path immediately before fsx
//! operates on it. An [`Expect`] is `Absent`, or `Present` with a [`Predicate`] whose
//! parts are all optional and all checked together. `mutate_with` checks the operation's
//! own path (`MutateOpts::expect`) and, for a rename, its destination
//! (`MutateOpts::expect_to`), at its first step, before the before-digest, the backup and
//! the operation.
//!
//! What each part proves, and what it does not:
//!
//! - `file_type`: a regular file, a directory or a symlink, read without following a
//!   final symlink. `sha256` implies a regular file, `stamp` a directory and
//!   `link_target` a symlink, so a contradictory predicate never holds.
//! - `identity`: the same file, by inode and, where the filesystem records one, birth
//!   time ([`FileId::same`]; the device is ignored, since it changes across mounts).
//!   Without a birth time, another file that reuses the inode is not told apart.
//! - `mode`: the same permission bits, setuid, setgid and sticky included.
//! - `link_target`: the same target, compared as written, never resolved.
//! - `sha256`: the same bytes; the one part that reads content.
//! - `stamp`: the same entries below a directory, by path, kind, size, mode and mtime
//!   ([`StampDigest`]). A file rewritten in place at the same size with its mtime put
//!   back is not seen, nor is the directory's own mode.
//!
//! The parts are checked cheapest first and the first that fails is reported, so a stale
//! identity is found without reading a byte. A refusal names the part, never the
//! content, a digest or a link target.
//!
//! The check is made immediately before the operation, which narrows the window in which
//! another writer can slip in; it is not an atomic compare-and-swap. Between the check and
//! the operation no Linux primitive closes the gap, and nothing here promises more.

use std::fmt;
use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use super::identity::FileId;
use super::mutate::{StampDigest, hash_file};

/// The kind of entry a predicate requires.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    Regular,
    Directory,
    Symlink,
}

/// The kind of entry found at a path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Found {
    Regular,
    Directory,
    Symlink,
    /// A FIFO, socket or device.
    Other,
}

impl Found {
    fn of(meta: &fs::Metadata) -> Self {
        let kind = meta.file_type();
        if kind.is_file() {
            Self::Regular
        } else if kind.is_dir() {
            Self::Directory
        } else if kind.is_symlink() {
            Self::Symlink
        } else {
            Self::Other
        }
    }

    fn is(self, kind: FileKind) -> bool {
        matches!(
            (self, kind),
            (Self::Regular, FileKind::Regular)
                | (Self::Directory, FileKind::Directory)
                | (Self::Symlink, FileKind::Symlink)
        )
    }
}

impl fmt::Display for FileKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Regular => "regular file",
            Self::Directory => "directory",
            Self::Symlink => "symlink",
        })
    }
}

impl fmt::Display for Found {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Regular => "regular file",
            Self::Directory => "directory",
            Self::Symlink => "symlink",
            Self::Other => "special file",
        })
    }
}

/// What must be true of a present entry. Every `Some` part is checked; `None` parts are
/// not, so `Predicate::default()` means "something is there".
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Predicate {
    pub file_type: Option<FileKind>,
    /// Compared with [`FileId::same`]: inode and birth time, the device ignored.
    pub identity: Option<FileId>,
    /// Implies a regular file.
    pub sha256: Option<[u8; 32]>,
    /// Permission bits, compared as `& 0o7777`.
    pub mode: Option<u32>,
    /// Implies a directory.
    pub stamp: Option<StampDigest>,
    /// Implies a symlink; compared exactly, never resolved.
    pub link_target: Option<PathBuf>,
}

/// A compare-and-swap precondition on a path, checked immediately before the operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expect {
    /// Nothing exists at the path.
    Absent,
    /// Something exists at the path, and the predicate holds for it.
    Present(Predicate),
}

/// The parts [`Expect::capture`] records, besides the file type it always records.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Parts {
    pub identity: bool,
    pub sha256: bool,
    pub mode: bool,
    pub stamp: bool,
    pub link_target: bool,
}

/// The first part of an [`Expect`] that did not hold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mismatch {
    /// `Absent`, and something is there.
    Exists,
    /// `Present`, and nothing is there.
    Missing,
    FileType {
        expected: FileKind,
        found: Found,
    },
    Identity,
    Sha256,
    Mode {
        expected: u32,
        found: u32,
    },
    Stamp,
    LinkTarget,
}

impl Mismatch {
    /// The part that failed, for callers that report it.
    pub const fn part(&self) -> &'static str {
        match self {
            Self::Exists => "absent",
            Self::Missing => "present",
            Self::FileType { .. } => "file_type",
            Self::Identity => "identity",
            Self::Sha256 => "sha256",
            Self::Mode { .. } => "mode",
            Self::Stamp => "stamp",
            Self::LinkTarget => "link_target",
        }
    }
}

/// Never the content, a digest or a link target: they may be private, and a digest in a
/// message invites comparing messages.
impl fmt::Display for Mismatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Exists => f.write_str("expected nothing at this path"),
            Self::Missing => f.write_str("expected something at this path"),
            Self::FileType { expected, found } => write!(f, "is a {found}, expected a {expected}"),
            Self::Identity => f.write_str("another file is at this path (its identity changed)"),
            Self::Sha256 => f.write_str("its content changed (hash mismatch)"),
            Self::Mode { expected, found } => {
                write!(f, "its mode is {found:04o}, expected {expected:04o}")
            }
            Self::Stamp => f.write_str("the directory's contents changed (stamp mismatch)"),
            Self::LinkTarget => f.write_str("the symlink points elsewhere"),
        }
    }
}

impl Expect {
    /// A regular file with this SHA-256.
    pub fn hash(sha256: [u8; 32]) -> Self {
        Self::Present(Predicate {
            file_type: Some(FileKind::Regular),
            sha256: Some(sha256),
            ..Predicate::default()
        })
    }

    /// Whether the expectation holds for what is at `path` now (`Ok(None)`), or the
    /// first part that does not. A path that cannot be read is an error: it never holds.
    pub fn check(&self, path: &Path) -> io::Result<Option<Mismatch>> {
        let meta = match fs::symlink_metadata(path) {
            Ok(meta) => meta,
            // ubs:ignore — an io::ErrorKind, not a secret
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                return Ok(match self {
                    Self::Absent => None,
                    Self::Present(_) => Some(Mismatch::Missing),
                });
            }
            Err(err) => return Err(err),
        };
        match self {
            Self::Absent => Ok(Some(Mismatch::Exists)),
            Self::Present(predicate) => predicate.check(path, &meta),
        }
    }

    /// What is at `path` now: `Absent` when nothing is, else `Present` with its file type
    /// and exactly the requested parts. A part that does not apply to the kind found
    /// (`sha256` of a non-regular file, `stamp` of a non-directory, `link_target` of a
    /// non-symlink), or a FIFO, socket or device, is an `InvalidInput` error: a predicate
    /// is never weaker than its caller asked for.
    pub fn capture(path: &Path, parts: Parts) -> io::Result<Self> {
        let meta = match fs::symlink_metadata(path) {
            Ok(meta) => meta,
            // ubs:ignore — an io::ErrorKind, not a secret
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Self::Absent),
            Err(err) => return Err(err),
        };
        let found = Found::of(&meta);
        let refuse = |what: &str| {
            Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{}: a {found} has no {what}", path.display()),
            ))
        };
        let file_type = match found {
            Found::Regular => FileKind::Regular,
            Found::Directory => FileKind::Directory,
            Found::Symlink => FileKind::Symlink,
            Found::Other => return refuse("predicate"),
        };
        if parts.sha256 && found != Found::Regular {
            return refuse("SHA-256");
        }
        if parts.stamp && found != Found::Directory {
            return refuse("stamp");
        }
        if parts.link_target && found != Found::Symlink {
            return refuse("link target");
        }
        Ok(Self::Present(Predicate {
            file_type: Some(file_type),
            identity: parts.identity.then(|| FileId::from_metadata(&meta)),
            sha256: parts.sha256.then(|| hash_file(path)).transpose()?,
            mode: parts.mode.then(|| meta.mode() & 0o7777),
            stamp: parts
                .stamp
                .then(|| StampDigest::compute(path))
                .transpose()?,
            link_target: parts.link_target.then(|| fs::read_link(path)).transpose()?,
        }))
    }
}

impl Predicate {
    /// Checks every given part against `meta` (read with `lstat`) and `path`, cheapest
    /// first: the file type, the identity and the mode from `meta`, then the link target
    /// (one `readlink`), the content (a hash) and the stamp (a walk).
    fn check(&self, path: &Path, meta: &fs::Metadata) -> io::Result<Option<Mismatch>> {
        let found = Found::of(meta);
        let kinds = [
            self.file_type,
            self.sha256.map(|_| FileKind::Regular),
            self.stamp.map(|_| FileKind::Directory),
            self.link_target.as_ref().map(|_| FileKind::Symlink),
        ];
        if let Some(expected) = kinds.into_iter().flatten().find(|kind| !found.is(*kind)) {
            return Ok(Some(Mismatch::FileType { expected, found }));
        }
        if let Some(expected) = self.identity
            && !FileId::same(Some(FileId::from_metadata(meta)), Some(expected))
        {
            return Ok(Some(Mismatch::Identity));
        }
        if let Some(expected) = self.mode {
            let (expected, found) = (expected & 0o7777, meta.mode() & 0o7777);
            if expected != found {
                return Ok(Some(Mismatch::Mode { expected, found }));
            }
        }
        if let Some(expected) = &self.link_target
            && fs::read_link(path)? != *expected
        {
            return Ok(Some(Mismatch::LinkTarget));
        }
        if let Some(expected) = &self.sha256
            // ubs:ignore — a content hash for compare-and-swap, not a secret
            && hash_file(path)? != *expected
        {
            return Ok(Some(Mismatch::Sha256));
        }
        if let Some(expected) = &self.stamp
            // ubs:ignore — a stat digest for compare-and-swap, not a secret
            && StampDigest::compute(path)? != *expected
        {
            return Ok(Some(Mismatch::Stamp));
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use std::fs::File;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::os::unix::net::UnixListener;
    use std::time::SystemTime;

    use sha2::{Digest as _, Sha256};

    use super::*;
    use crate::fsx::files_hashed;

    fn root() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        (dir, root)
    }

    fn sha(bytes: &[u8]) -> [u8; 32] {
        Sha256::digest(bytes).into()
    }

    fn file(path: &Path, bytes: &[u8], mode: u32) {
        fs::write(path, bytes).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
    }

    /// Replaces the file at `path` with another of the same bytes and mode, by renaming a
    /// copy over it: only its identity changes.
    fn replace_with_a_copy(path: &Path) {
        let copy = path.with_extension("copy");
        fs::copy(path, &copy).unwrap();
        let mode = fs::metadata(path).unwrap().permissions().mode();
        fs::set_permissions(&copy, fs::Permissions::from_mode(mode)).unwrap();
        fs::rename(&copy, path).unwrap();
    }

    /// Rewrites `path` in place with `bytes`, its mtime put back.
    fn rewrite_keeping_mtime(path: &Path, bytes: &[u8]) {
        let mtime: SystemTime = fs::metadata(path).unwrap().modified().unwrap();
        fs::write(path, bytes).unwrap();
        File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(mtime)
            .unwrap();
    }

    fn all_parts() -> Parts {
        Parts {
            identity: true,
            sha256: true,
            mode: true,
            stamp: true,
            link_target: true,
        }
    }

    /// Every part that applies to the entry at `path`.
    fn applicable(path: &Path) -> Parts {
        let found = Found::of(&fs::symlink_metadata(path).unwrap());
        Parts {
            sha256: found == Found::Regular,
            stamp: found == Found::Directory,
            link_target: found == Found::Symlink,
            ..all_parts()
        }
    }

    fn present(predicate: Predicate) -> Expect {
        Expect::Present(predicate)
    }

    #[test]
    fn absent_and_present() {
        let (_dir, root) = root();
        let (there, gone) = (root.join("there"), root.join("gone"));
        file(&there, b"x", 0o644);
        let any = present(Predicate::default());
        for (case, expect, path, want) in [
            ("absent_path_absent_expect", Expect::Absent, &gone, None),
            (
                "present_path_absent_expect",
                Expect::Absent,
                &there,
                Some(Mismatch::Exists),
            ),
            (
                "absent_path_present_expect",
                any.clone(),
                &gone,
                Some(Mismatch::Missing),
            ),
            ("present_path_empty_predicate", any.clone(), &there, None),
        ] {
            let got = expect.check(path).unwrap();
            assert_eq!(
                got, want,
                "{case}: check({path:?}) with {expect:?} = {got:?}, expected {want:?}"
            );
        }
    }

    #[test]
    fn file_types() {
        let (_dir, root) = root();
        let regular = root.join("regular");
        file(&regular, b"x", 0o644);
        let directory = root.join("directory");
        fs::create_dir(&directory).unwrap();
        let link = root.join("link");
        symlink("regular", &link).unwrap();
        let socket = root.join("socket");
        let _listener = UnixListener::bind(&socket).unwrap();
        let entries = [
            (&regular, Found::Regular),
            (&directory, Found::Directory),
            (&link, Found::Symlink),
            (&socket, Found::Other),
        ];
        for kind in [FileKind::Regular, FileKind::Directory, FileKind::Symlink] {
            for (path, found) in entries {
                let expect = present(Predicate {
                    file_type: Some(kind),
                    ..Predicate::default()
                });
                let want = (!found.is(kind)).then_some(Mismatch::FileType {
                    expected: kind,
                    found,
                });
                let got = expect.check(path).unwrap();
                assert_eq!(
                    got, want,
                    "{kind:?} against {found:?}: check({path:?}) = {got:?}, expected {want:?}"
                );
            }
        }
    }

    #[test]
    fn identity_refuses_another_file_of_the_same_mode_and_content() {
        let (_dir, root) = root();
        let f = root.join("f");
        file(&f, b"x", 0o644);
        let parts = Parts {
            identity: true,
            sha256: true,
            mode: true,
            ..Parts::default()
        };
        let expect = Expect::capture(&f, parts).unwrap();
        replace_with_a_copy(&f);
        let got = expect.check(&f).unwrap();
        assert_eq!(
            got,
            Some(Mismatch::Identity),
            "check({f:?}) with {expect:?}"
        );
    }

    fn captured_identity(path: &Path) -> FileId {
        let parts = Parts {
            identity: true,
            ..Parts::default()
        };
        let predicate = match Expect::capture(path, parts).unwrap() {
            Expect::Present(predicate) => Some(predicate),
            Expect::Absent => None,
        };
        predicate.and_then(|p| p.identity).unwrap()
    }

    fn with_identity(id: FileId) -> Expect {
        present(Predicate {
            identity: Some(id),
            ..Predicate::default()
        })
    }

    #[test]
    fn identity_survives_a_renumbered_device() {
        let (_dir, root) = root();
        let f = root.join("f");
        file(&f, b"x", 0o644);
        let mut id = captured_identity(&f);
        id.dev ^= 0x5a5a;
        let got = with_identity(id).check(&f).unwrap();
        assert_eq!(got, None, "check({f:?}) with dev rewritten to {id:?}");
    }

    #[test]
    fn identity_refuses_a_reused_inode_with_another_birth_time() {
        let (_dir, root) = root();
        let f = root.join("f");
        file(&f, b"x", 0o644);
        let mut id = captured_identity(&f);
        let want = match id.born_ns {
            Some(born) => {
                id.born_ns = Some(born - 1_000_000_000);
                Some(Mismatch::Identity)
            }
            // No birth time on this filesystem: decided by inode alone.
            None => None,
        };
        let got = with_identity(id).check(&f).unwrap();
        assert_eq!(got, want, "check({f:?}) with {id:?}");
    }

    #[test]
    fn sha256_refuses_a_same_size_edit() {
        let (_dir, root) = root();
        let f = root.join("f");
        file(&f, b"aaaa", 0o644);
        let expect = Expect::hash(sha(b"aaaa"));
        assert_eq!(expect.check(&f).unwrap(), None, "unchanged");
        rewrite_keeping_mtime(&f, b"aaab");
        let got = expect.check(&f).unwrap();
        assert_eq!(
            got,
            Some(Mismatch::Sha256),
            "check({f:?}) after aaaa → aaab"
        );
    }

    #[test]
    fn sha256_hashes_once_and_only_when_asked() {
        let (_dir, root) = root();
        let f = root.join("f");
        file(&f, b"aaaa", 0o644);
        for (case, expect, hashed) in [
            ("with_sha256", Expect::hash(sha(b"aaaa")), 1),
            (
                "without_sha256",
                present(Predicate {
                    file_type: Some(FileKind::Regular),
                    mode: Some(0o644),
                    ..Predicate::default()
                }),
                0,
            ),
        ] {
            let before = files_hashed();
            assert_eq!(expect.check(&f).unwrap(), None, "{case}");
            assert_eq!(files_hashed() - before, hashed, "{case}: files hashed");
        }
    }

    #[test]
    fn mode_refuses_a_mode_change() {
        let (_dir, root) = root();
        let f = root.join("f");
        file(&f, b"x", 0o644);
        let parts = Parts {
            mode: true,
            ..Parts::default()
        };
        let expect = Expect::capture(&f, parts).unwrap();
        fs::set_permissions(&f, fs::Permissions::from_mode(0o600)).unwrap();
        let got = expect.check(&f).unwrap();
        let want = Mismatch::Mode {
            expected: 0o644,
            found: 0o600,
        };
        assert_eq!(got, Some(want), "check({f:?}) after chmod 0600");
    }

    #[test]
    fn mode_counts_the_special_bits() {
        let (_dir, root) = root();
        let f = root.join("f");
        file(&f, b"x", 0o755);
        let expect = present(Predicate {
            mode: Some(0o4755),
            ..Predicate::default()
        });
        let got = expect.check(&f).unwrap();
        let want = Mismatch::Mode {
            expected: 0o4755,
            found: 0o755,
        };
        assert_eq!(got, Some(want), "check({f:?}) of 0755 against 04755");
    }

    /// `<root>/d` holding `a` (`aaaa`) and `sub/b`.
    fn directory(root: &Path) -> PathBuf {
        let d = root.join("d");
        fs::create_dir_all(d.join("sub")).unwrap();
        file(&d.join("a"), b"aaaa", 0o644);
        file(&d.join("sub/b"), b"b", 0o644);
        d
    }

    #[test]
    fn stamp_refuses_a_directory_content_change() {
        type Change = fn(&Path);
        let cases: [(&str, Change); 4] = [
            ("an_added_file", |d| file(&d.join("new"), b"", 0o644)),
            ("a_removed_file", |d| fs::remove_file(d.join("a")).unwrap()),
            ("a_size_change_inside", |d| {
                rewrite_keeping_mtime(&d.join("sub/b"), b"bb");
            }),
            ("a_renamed_entry", |d| {
                fs::rename(d.join("a"), d.join("z")).unwrap();
            }),
        ];
        for (case, change) in cases {
            let (_dir, root) = root();
            let d = directory(&root);
            let parts = Parts {
                stamp: true,
                ..Parts::default()
            };
            let expect = Expect::capture(&d, parts).unwrap();
            change(&d);
            let got = expect.check(&d).unwrap();
            assert_eq!(got, Some(Mismatch::Stamp), "{case}: check({d:?})");
        }
    }

    /// The documented limit, pinned so it stays visible.
    #[test]
    fn stamp_does_not_see_a_same_size_rewrite_with_its_mtime_put_back() {
        let (_dir, root) = root();
        let d = directory(&root);
        let parts = Parts {
            stamp: true,
            ..Parts::default()
        };
        let expect = Expect::capture(&d, parts).unwrap();
        rewrite_keeping_mtime(&d.join("a"), b"aaab");
        assert_eq!(expect.check(&d).unwrap(), None, "check({d:?})");
    }

    #[test]
    fn link_target_refuses_a_retargeted_symlink() {
        let (_dir, root) = root();
        let l = root.join("l");
        symlink("a", &l).unwrap();
        let parts = Parts {
            link_target: true,
            ..Parts::default()
        };
        let expect = Expect::capture(&l, parts).unwrap();
        fs::remove_file(&l).unwrap();
        symlink("b", &l).unwrap();
        let got = expect.check(&l).unwrap();
        assert_eq!(got, Some(Mismatch::LinkTarget), "check({l:?}) after l → b");
    }

    #[test]
    fn a_symlink_replaced_by_a_file_is_a_file_type_mismatch() {
        let (_dir, root) = root();
        let l = root.join("l");
        symlink("a", &l).unwrap();
        let parts = Parts {
            link_target: true,
            ..Parts::default()
        };
        let expect = Expect::capture(&l, parts).unwrap();
        fs::remove_file(&l).unwrap();
        file(&l, b"a", 0o644);
        let got = expect.check(&l).unwrap();
        let want = Mismatch::FileType {
            expected: FileKind::Symlink,
            found: Found::Regular,
        };
        assert_eq!(got, Some(want), "check({l:?}) after a file replaced it");
    }

    #[test]
    fn implied_types() {
        let (_dir, root) = root();
        let f = root.join("f");
        file(&f, b"x", 0o644);
        let d = directory(&root);
        for (case, predicate, path, want) in [
            (
                "sha256_on_a_directory",
                Predicate {
                    sha256: Some(sha(b"x")),
                    ..Predicate::default()
                },
                &d,
                (FileKind::Regular, Found::Directory),
            ),
            (
                "stamp_on_a_file",
                Predicate {
                    stamp: Some(StampDigest::compute(&d).unwrap()),
                    ..Predicate::default()
                },
                &f,
                (FileKind::Directory, Found::Regular),
            ),
            (
                "link_target_on_a_file",
                Predicate {
                    link_target: Some(PathBuf::from("f")),
                    ..Predicate::default()
                },
                &f,
                (FileKind::Symlink, Found::Regular),
            ),
        ] {
            let got = present(predicate).check(path).unwrap();
            let (expected, found) = want;
            assert_eq!(
                got,
                Some(Mismatch::FileType { expected, found }),
                "{case}: check({path:?})"
            );
        }
    }

    #[test]
    fn contradictory_predicates_never_hold() {
        let (_dir, root) = root();
        let f = root.join("f");
        file(&f, b"x", 0o644);
        let d = directory(&root);
        let expect = present(Predicate {
            file_type: Some(FileKind::Directory),
            sha256: Some(sha(b"x")),
            ..Predicate::default()
        });
        for path in [&d, &f] {
            let got = expect.check(path).unwrap();
            assert!(
                matches!(got, Some(Mismatch::FileType { .. })),
                "check({path:?}) with {expect:?} = {got:?}, expected a file type mismatch"
            );
        }
    }

    #[test]
    fn capture_records_what_was_asked() {
        let (_dir, root) = root();
        let f = root.join("f");
        file(&f, b"x", 0o640);
        let d = directory(&root);
        let l = root.join("l");
        symlink("f", &l).unwrap();
        for (case, path, parts, want) in [
            (
                "file_identity_sha256_mode",
                &f,
                Parts {
                    identity: true,
                    sha256: true,
                    mode: true,
                    ..Parts::default()
                },
                present(Predicate {
                    file_type: Some(FileKind::Regular),
                    identity: FileId::of(&f),
                    sha256: Some(sha(b"x")),
                    mode: Some(0o640),
                    ..Predicate::default()
                }),
            ),
            (
                "directory_identity_stamp",
                &d,
                Parts {
                    identity: true,
                    stamp: true,
                    ..Parts::default()
                },
                present(Predicate {
                    file_type: Some(FileKind::Directory),
                    identity: FileId::of(&d),
                    stamp: Some(StampDigest::compute(&d).unwrap()),
                    ..Predicate::default()
                }),
            ),
            (
                "symlink_identity_link_target",
                &l,
                Parts {
                    identity: true,
                    link_target: true,
                    ..Parts::default()
                },
                present(Predicate {
                    file_type: Some(FileKind::Symlink),
                    identity: FileId::of(&l),
                    link_target: Some(PathBuf::from("f")),
                    ..Predicate::default()
                }),
            ),
            (
                "absent_is_absent",
                &root.join("gone"),
                all_parts(),
                Expect::Absent,
            ),
        ] {
            let got = Expect::capture(path, parts).unwrap();
            assert_eq!(got, want, "{case}: capture({path:?}, {parts:?})");
        }
    }

    #[test]
    fn capture_refuses() {
        let (_dir, root) = root();
        let f = root.join("f");
        file(&f, b"x", 0o644);
        let d = directory(&root);
        let socket = root.join("socket");
        let _listener = UnixListener::bind(&socket).unwrap();
        let only = |part: fn(&mut Parts)| {
            let mut parts = Parts::default();
            part(&mut parts);
            parts
        };
        for (case, path, parts) in [
            ("sha256_of_a_directory", &d, only(|p| p.sha256 = true)),
            ("stamp_of_a_file", &f, only(|p| p.stamp = true)),
            ("link_target_of_a_file", &f, only(|p| p.link_target = true)),
            ("a_socket", &socket, Parts::default()),
        ] {
            let got = Expect::capture(path, parts);
            assert!(
                matches!(&got, Err(err) if err.kind() == io::ErrorKind::InvalidInput),
                "{case}: capture({path:?}, {parts:?}) = {got:?}, expected InvalidInput"
            );
        }
    }

    #[test]
    fn capture_then_check_holds_when_nothing_changed() {
        let (_dir, root) = root();
        let f = root.join("f");
        file(&f, b"x", 0o644);
        let d = directory(&root);
        let l = root.join("l");
        symlink("f", &l).unwrap();
        for path in [&f, &d, &l] {
            let parts = applicable(path);
            let expect = Expect::capture(path, parts).unwrap();
            let got = expect.check(path).unwrap();
            assert_eq!(
                got, None,
                "check({path:?}) right after capture with {parts:?}"
            );
        }
    }

    #[test]
    fn checks_run_cheapest_first() {
        let (_dir, root) = root();
        let f = root.join("f");
        file(&f, b"old", 0o644);
        let expect = Expect::capture(&f, applicable(&f)).unwrap();
        let other = root.join("other");
        file(&other, b"new", 0o644);
        fs::rename(&other, &f).unwrap();
        let before = files_hashed();
        let got = expect.check(&f).unwrap();
        assert_eq!(got, Some(Mismatch::Identity), "check({f:?})");
        assert_eq!(files_hashed(), before, "a stale identity is found unhashed");
    }

    #[test]
    fn mismatch_messages_name_the_part_and_carry_no_content() {
        let (_dir, root) = root();
        let f = root.join("f");
        file(&f, b"SECRETCONTENT", 0o644);
        let l = root.join("l");
        symlink("SECRETTARGET", &l).unwrap();
        let file_expect = Expect::capture(&f, applicable(&f)).unwrap();
        let parts = Parts {
            link_target: true,
            ..Parts::default()
        };
        let link_expect = Expect::capture(&l, parts).unwrap();
        rewrite_keeping_mtime(&f, b"SECRETCONTENU");
        fs::remove_file(&l).unwrap();
        symlink("SECRETTARGEU", &l).unwrap();
        let observed = [
            file_expect.check(&f).unwrap().unwrap(),
            link_expect.check(&l).unwrap().unwrap(),
        ];
        assert_eq!(observed[0], Mismatch::Sha256, "the planted file");
        assert_eq!(observed[1], Mismatch::LinkTarget, "the planted link");
        let every = [
            (Mismatch::Exists, "absent", "expected nothing at this path"),
            (
                Mismatch::Missing,
                "present",
                "expected something at this path",
            ),
            (
                Mismatch::FileType {
                    expected: FileKind::Directory,
                    found: Found::Other,
                },
                "file_type",
                "is a special file, expected a directory",
            ),
            (
                Mismatch::Identity,
                "identity",
                "another file is at this path (its identity changed)",
            ),
            (
                Mismatch::Sha256,
                "sha256",
                "its content changed (hash mismatch)",
            ),
            (
                Mismatch::Mode {
                    expected: 0o644,
                    found: 0o4755,
                },
                "mode",
                "its mode is 4755, expected 0644",
            ),
            (
                Mismatch::Stamp,
                "stamp",
                "the directory's contents changed (stamp mismatch)",
            ),
            (
                Mismatch::LinkTarget,
                "link_target",
                "the symlink points elsewhere",
            ),
        ];
        for (mismatch, part, text) in &every {
            assert_eq!(mismatch.part(), *part, "{mismatch:?}");
            assert_eq!(mismatch.to_string(), *text, "{mismatch:?}");
        }
        let hex_run = |s: &str| {
            s.as_bytes()
                .windows(64)
                .any(|w| w.iter().all(u8::is_ascii_hexdigit))
        };
        for message in observed
            .iter()
            .chain(every.iter().map(|(m, _, _)| m))
            .map(ToString::to_string)
        {
            assert!(
                !message.contains("SECRET") && !hex_run(&message),
                "{message:?} carries content, a target or a digest"
            );
        }
    }

    #[test]
    fn expect_hash_is_a_regular_file_with_this_sha256() {
        let digest = sha(b"x");
        let want = present(Predicate {
            file_type: Some(FileKind::Regular),
            sha256: Some(digest),
            ..Predicate::default()
        });
        assert_eq!(Expect::hash(digest), want);
    }
}
