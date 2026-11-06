//! Reading an installed skill's `SKILL.md` (M4-D8, M4-D50) without trusting it: a skill
//! folder sits in a user's dotfiles, so its `SKILL.md` may be a symlink (never followed:
//! a later write must not go through it), a FIFO a tool left (never opened: it would block
//! the command) or a huge file (never read past [`DISCOVERY_FILE_MAX_BYTES`]).
//! `install-skill` decides from it whether to write, refuse or keep an edited copy, and
//! the doctor's skill-drift check whether a skill is current, outdated or edited, each
//! with [`super::stamp::inspect`]. It lives apart from `stamp.rs`, which `build.rs`
//! compiles with `std` and `sha2` alone.

use std::io::{self, Read};
use std::path::{Path, PathBuf};

use rustix::fs::{Mode, OFlags};

use crate::limits::DISCOVERY_FILE_MAX_BYTES;

/// What is at a skill's `SKILL.md`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillFile {
    Missing,
    /// A symlink, dangling or not, never followed; `link_target` as `read_link` gives it.
    Symlink {
        link_target: PathBuf,
    },
    /// A directory, FIFO, socket or device: never opened for reading.
    NotAFile,
    /// A regular file longer than [`DISCOVERY_FILE_MAX_BYTES`], never read past the bound
    /// (a guide is far smaller, so callers treat it as edited).
    TooLarge {
        len: u64,
    },
    Bytes(Vec<u8>),
}

/// Reads `path` as a skill's `SKILL.md`: classified from its own `lstat`, opened with
/// `O_NOFOLLOW` and `O_NONBLOCK` only when it is a regular file within the bound, and read
/// through at most one byte more than the bound. A path that became a symlink or vanished
/// between the two (`ELOOP`, `ENOENT` on open) is classified again, once.
pub fn read_skill_file(path: &Path) -> io::Result<SkillFile> {
    match read_once(path) {
        Err(err) if raced(&err) => read_once(path),
        read => read,
    }
}

/// Whether `err` from opening a path just classified says it changed in between.
fn raced(err: &io::Error) -> bool {
    err.kind() == io::ErrorKind::NotFound
        || err.raw_os_error() == Some(rustix::io::Errno::LOOP.raw_os_error())
}

fn read_once(path: &Path) -> io::Result<SkillFile> {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(SkillFile::Missing),
        Err(err) => return Err(err),
    };
    if meta.file_type().is_symlink() {
        return Ok(SkillFile::Symlink {
            link_target: std::fs::read_link(path)?,
        });
    }
    if !meta.is_file() {
        return Ok(SkillFile::NotAFile);
    }
    if meta.len() > DISCOVERY_FILE_MAX_BYTES {
        return Ok(SkillFile::TooLarge { len: meta.len() });
    }
    let fd = rustix::fs::open(
        path,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
        Mode::empty(),
    )?;
    let file = std::fs::File::from(fd);
    if !file.metadata()?.is_file() {
        return Ok(SkillFile::NotAFile);
    }
    let mut bytes = Vec::new();
    file.take(DISCOVERY_FILE_MAX_BYTES + 1)
        .read_to_end(&mut bytes)?;
    let len = bytes.len() as u64;
    if len > DISCOVERY_FILE_MAX_BYTES {
        // It grew since its `lstat`: what was read is all that is known of its length.
        return Ok(SkillFile::TooLarge { len });
    }
    Ok(SkillFile::Bytes(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A name, how to make it under the test dir, and what it must read as.
    type Case = (&'static str, fn(&Path) -> PathBuf, fn(&Path) -> SkillFile);

    fn file(dir: &Path, bytes: &[u8]) -> PathBuf {
        let path = dir.join("SKILL.md");
        std::fs::write(&path, bytes).unwrap();
        path
    }

    fn bound() -> usize {
        usize::try_from(DISCOVERY_FILE_MAX_BYTES).unwrap()
    }

    #[test]
    fn read_skill_file_classifies_every_kind() {
        let cases: Vec<Case> = vec![
            ("missing", |d| d.join("SKILL.md"), |_| SkillFile::Missing),
            (
                "regular_file",
                |d| file(d, b"---\nname: omaddons\n---\nbody\n"),
                |_| SkillFile::Bytes(b"---\nname: omaddons\n---\nbody\n".to_vec()),
            ),
            (
                "symlink_to_file_relative",
                |d| {
                    std::fs::write(d.join("real.md"), b"guide\n").unwrap();
                    let link = d.join("SKILL.md");
                    std::os::unix::fs::symlink("real.md", &link).unwrap();
                    link
                },
                |_| SkillFile::Symlink {
                    link_target: PathBuf::from("real.md"),
                },
            ),
            (
                "symlink_to_file_absolute",
                |d| {
                    std::fs::write(d.join("real.md"), b"guide\n").unwrap();
                    let link = d.join("SKILL.md");
                    std::os::unix::fs::symlink(d.join("real.md"), &link).unwrap();
                    link
                },
                |d| SkillFile::Symlink {
                    link_target: d.join("real.md"),
                },
            ),
            (
                "dangling_symlink",
                |d| {
                    let link = d.join("SKILL.md");
                    std::os::unix::fs::symlink("gone.md", &link).unwrap();
                    link
                },
                |_| SkillFile::Symlink {
                    link_target: PathBuf::from("gone.md"),
                },
            ),
            (
                "directory",
                |d| {
                    let path = d.join("SKILL.md");
                    std::fs::create_dir(&path).unwrap();
                    path
                },
                |_| SkillFile::NotAFile,
            ),
            (
                "fifo",
                |d| {
                    let path = d.join("SKILL.md");
                    rustix::fs::mknodat(
                        rustix::fs::CWD,
                        &path,
                        rustix::fs::FileType::Fifo,
                        Mode::from_raw_mode(0o644),
                        0,
                    )
                    .unwrap();
                    path
                },
                |_| SkillFile::NotAFile,
            ),
            (
                "exactly_the_bound",
                |d| file(d, &vec![b'x'; bound()]),
                |_| SkillFile::Bytes(vec![b'x'; bound()]),
            ),
            (
                "one_byte_over_the_bound",
                |d| file(d, &vec![b'x'; bound() + 1]),
                |_| SkillFile::TooLarge {
                    len: DISCOVERY_FILE_MAX_BYTES + 1,
                },
            ),
        ];
        for (case, make, want) in cases {
            let dir = tempfile::tempdir().unwrap();
            let root = std::fs::canonicalize(dir.path()).unwrap();
            let path = make(&root);
            // A FIFO opened for reading would block here: the test would hang, not fail.
            let got = read_skill_file(&path).unwrap();
            let shown = |f: &SkillFile| match f {
                SkillFile::Bytes(bytes) => format!("Bytes({} bytes)", bytes.len()),
                other => format!("{other:?}"),
            };
            assert!(
                got == want(&root),
                "{case}: got {}, expected {}",
                shown(&got),
                shown(&want(&root))
            );
        }
    }

    #[test]
    fn read_skill_file_never_reads_past_the_bound() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("SKILL.md");
        let len = 3 * 1024 * 1024;
        std::fs::File::create(&path).unwrap().set_len(len).unwrap();
        assert_eq!(
            read_skill_file(&path).unwrap(),
            SkillFile::TooLarge { len },
            "a sparse 3 MiB file is judged by its length, never read"
        );
    }
}
