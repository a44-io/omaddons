//! `file:` refs (vision §5.4): a zip someone sent, or a folder the user unpacked or
//! develops in, often the only option for legacy clients. No network and no writes:
//! the archive stays where it is (the user owns it; undo uses snapshots), and a
//! directory is read in place, never re-zipped.
//!
//! The release identity follows the content, so an unchanged input plans as already
//! current: a zip's SHA-256, or a hash of a directory's `(path, size, mtime)` listing
//! (`.git` excluded, as inspection skips it). The version is left empty for the engine
//! to read from the TOC (`version_confidence: toc`).

use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use sha2::{Digest, Sha256};

use super::{Source, SourceId, Wanted};
use crate::engine::inspect::{ArchiveFault, archive_error};
use crate::error::{Error, ErrorCode, Result, Suggestion};
use crate::model::{Asset, AssetLocation, Channel, Ref, Release, ReleaseSet};
use crate::net::Http;

/// The `file:` source.
#[derive(Debug, Default)]
pub struct LocalSource;

impl Source for LocalSource {
    fn id(&self) -> SourceId {
        SourceId::Local
    }

    fn fetch(&self, wanted: &[Wanted], _: &dyn Http) -> Vec<Result<ReleaseSet>> {
        wanted.iter().map(|w| release_set(&w.r#ref)).collect()
    }
}

fn release_set(r: &Ref) -> Result<ReleaseSet> {
    let Ref::File { path } = r else {
        return Err(Error::internal(format!("the local source got {r}")));
    };
    let meta = std::fs::metadata(path).map_err(|_| {
        Error::new(
            ErrorCode::NotFound,
            format!("{} does not exist", path.display()),
            Suggestion::command(["omaddons", "help", "install"]),
        )
    })?;
    let (location, identity) = if meta.is_dir() {
        (
            AssetLocation::LocalDir(path.clone()),
            format!("listing:{}", listing_hash(path)),
        )
    } else if meta.is_file() && is_zip(path) {
        (
            AssetLocation::LocalZip(path.clone()),
            format!("sha256:{}", file_hash(path)?),
        )
    } else {
        return Err(archive_error(
            ArchiveFault::Malformed,
            format!("{} is not a zip archive or a directory", path.display()),
            serde_json::json!({ "reason": "not_a_zip", "path": path.display().to_string() }),
        ));
    };
    let filename = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    Ok(ReleaseSet {
        r#ref: r.clone(),
        releases: vec![Release {
            release_id: format!("{r}@{identity}"),
            version: String::new(),
            channel: Channel::Stable,
            published_at: None,
            assets: vec![Asset {
                filename,
                location,
                size: meta.is_file().then_some(meta.len()),
                expected_hash: None,
                flavors: Vec::new(),
                name_flavors: Vec::new(),
                nolib: false,
            }],
            changelog_url: None,
        }],
    })
}

/// A local file header's magic; an empty archive (`PK\x05\x06`) is inspection's to
/// reject.
fn is_zip(path: &Path) -> bool {
    let mut magic = [0u8; 4];
    std::fs::File::open(path)
        .and_then(|mut f| f.read_exact(&mut magic))
        .is_ok_and(|()| &magic == b"PK\x03\x04" || &magic == b"PK\x05\x06")
}

fn file_hash(path: &Path) -> Result<String> {
    let mut file = std::fs::File::open(path)
        .map_err(|e| Error::io(path, &e, Suggestion::text("check the file, then retry")))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = file
            .read(&mut buf)
            .map_err(|e| Error::io(path, &e, Suggestion::text("check the file, then retry")))?;
        if n == 0 {
            break;
        }
        hasher.update(buf.get(..n).unwrap_or_default());
    }
    Ok(hex(&hasher.finalize()))
}

/// SHA-256 of the sorted `<path>\0<size>\0<mtime_ns>\n` lines of every regular file.
fn listing_hash(dir: &Path) -> String {
    let mut lines = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(at) = pending.pop() {
        let Ok(listing) = std::fs::read_dir(&at) else {
            continue;
        };
        for entry in listing.filter_map(std::result::Result::ok) {
            let path = entry.path();
            let Ok(meta) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if meta.is_dir() {
                if entry.file_name() != ".git" {
                    pending.push(path);
                }
            } else {
                let rel = path
                    .strip_prefix(dir)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .into_owned();
                let mtime =
                    i128::from(meta.mtime()) * 1_000_000_000 + i128::from(meta.mtime_nsec());
                lines.push(format!("{rel}\0{}\0{mtime}\n", meta.len()));
            }
        }
    }
    lines.sort();
    hex(&Sha256::digest(lines.concat().as_bytes()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fetch(path: &Path) -> Result<ReleaseSet> {
        release_set(&Ref::File {
            path: path.to_path_buf(),
        })
    }

    #[test]
    fn zips_and_directories() {
        let dir = tempfile::tempdir().unwrap();
        let zip = dir.path().join("Addon.zip");
        std::fs::write(&zip, b"PK\x03\x04rest").unwrap();
        let set = fetch(&zip).unwrap();
        let release = &set.releases[0];
        assert!(
            release
                .release_id
                .starts_with(&format!("file:{}@sha256:", zip.display())),
            "{}",
            release.release_id
        );
        assert_eq!(
            (release.version.as_str(), release.channel),
            ("", Channel::Stable)
        );
        assert_eq!(
            release.assets[0].location,
            AssetLocation::LocalZip(zip.clone())
        );
        assert_eq!(release.assets[0].filename, "Addon.zip");

        let addon = dir.path().join("MyAddon");
        std::fs::create_dir_all(addon.join(".git")).unwrap();
        std::fs::write(addon.join("MyAddon.toc"), "## Interface: 120105\n").unwrap();
        let first = fetch(&addon).unwrap().releases[0].release_id.clone();
        assert_eq!(
            fetch(&addon).unwrap().releases[0].release_id,
            first,
            "stable"
        );
        std::fs::write(addon.join(".git/HEAD"), "ref").unwrap();
        assert_eq!(
            fetch(&addon).unwrap().releases[0].release_id,
            first,
            ".git is not content"
        );
        std::fs::write(
            addon.join("MyAddon.toc"),
            "## Interface: 120105\n## Version: 2\n",
        )
        .unwrap();
        assert_ne!(
            fetch(&addon).unwrap().releases[0].release_id,
            first,
            "a change changes it"
        );
        assert_eq!(
            fetch(&addon).unwrap().releases[0].assets[0].location,
            AssetLocation::LocalDir(addon.clone())
        );

        let empty = dir.path().join("empty.zip");
        std::fs::write(&empty, b"").unwrap();
        let text = dir.path().join("notes.txt");
        std::fs::write(&text, "hello").unwrap();
        for bad in [&empty, &text] {
            let err = fetch(bad).unwrap_err();
            assert_eq!(
                (err.code, err.exit().code()),
                (ErrorCode::BadArchive, 1),
                "{bad:?}"
            );
        }
        let err = fetch(&dir.path().join("missing.zip")).unwrap_err();
        assert_eq!((err.code, err.exit().code()), (ErrorCode::NotFound, 3));
    }

    #[test]
    fn the_same_zip_keeps_its_identity() {
        let dir = tempfile::tempdir().unwrap();
        let zip = dir.path().join("a.zip");
        std::fs::write(&zip, b"PK\x03\x04one").unwrap();
        let one = fetch(&zip).unwrap().releases[0].release_id.clone();
        assert_eq!(fetch(&zip).unwrap().releases[0].release_id, one);
        std::fs::write(&zip, b"PK\x03\x04two").unwrap();
        assert_ne!(fetch(&zip).unwrap().releases[0].release_id, one);
    }
}
