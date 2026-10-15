//! Staging (vision §5.5 step 5): the inspected addon folders extracted to
//! `<client dir>/Interface/.omaddons/stage/<tx>/<Folder>/`, before the global lock.
//!
//! The stage dir is on the same filesystem as `Interface/AddOns/`, so the swap is a
//! rename; it is outside the directory the client scans, so a half-extracted folder is
//! never visible; and it is per transaction. Its liveness is the transaction's in-flight
//! lock (D23): a stage dir whose lock is free is an orphan.
//!
//! Files are written through fsx with deferred durability, then one `syncfs` makes the
//! whole stage durable: per-file fsyncs would cost seconds for addons with thousands of
//! files. Each file's size and SHA-256 are checked against the inspection as it is
//! written, modes are always 0644 (files) and 0755 (directories), and the per-file
//! index (`path, size, mtime_ns, sha256`) is produced here. A folder's tree hash is
//! fsx's [`crate::fsx::TreeDigest`]: SHA-256 over the path-sorted lines
//! `<path>\0<size>\0<sha256 hex>\n`.

use std::collections::BTreeSet;
use std::io::{self, Read};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use serde_json::json;
use sha2::{Digest, Sha256};

use super::inspect::{ArchiveFault, FileSource, Inspected, archive_error};
use crate::error::{Error, ErrorCode, Result, Suggestion};
use crate::fsx::{
    self, Content, Durability, InflightLock, MutateOpts, Op, ScopeTable, WriteSession, Writer,
    ensure_dir_all, failpoint, mutate, mutate_with,
};
use crate::model::{FolderName, Target, TxId};
use crate::state::files_index::{FileEntry, FolderIndex};
use crate::state::paths::ClientPaths;

const FILE_MODE: u32 = 0o644;
const DIR_MODE: u32 = 0o755;

/// A transaction's staged folders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Staged {
    pub tx: TxId,
    pub stage_dir: PathBuf,
    pub folders: Vec<StagedFolder>,
}

/// One staged folder and its per-file index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedFolder {
    pub name: FolderName,
    /// `stage/<tx>/<name>`.
    pub path: PathBuf,
    /// Its files and tree hash, as `targets/<key>.files.json` records them.
    pub index: FolderIndex,
}

/// Extracts `inspected` for `target` into the transaction's stage dir. `blob` is the
/// zip the inspection read; directory inputs (`FileSource::Disk`) need none. The
/// in-flight lock proves the transaction is live; `scopes` must cover the client dir.
/// On error the stage dir may hold partial content, for the caller to prune.
pub fn stage(
    inflight: &InflightLock,
    scopes: &ScopeTable,
    target: &Target,
    blob: Option<&Path>,
    inspected: &Inspected,
) -> Result<Staged> {
    let tx = inflight.tx();
    let client = ClientPaths::new(&target.client_dir);
    let stage_dir = client.stage(&tx);
    let fail =
        |e: io::Error| Error::io(&stage_dir, &e, Suggestion::command(["omaddons", "status"]));
    failpoint::hit("engine.stage.begin").map_err(fail)?;

    let mut s = WriteSession::untracked(Writer::Mutating, scopes, "stage");
    if let Some(parent) = stage_dir.parent() {
        ensure_dir_all(&mut s, parent)?;
    }
    create_dir(&mut s, &stage_dir)?;
    same_filesystem(&stage_dir, &client)?;

    let mut archive = match blob {
        Some(blob) => Some(open_zip(blob)?),
        None => None,
    };
    let mut folders = Vec::with_capacity(inspected.folders.len());
    let mut first = true;
    for folder in &inspected.folders {
        let folder_dir = stage_dir.join(folder.name.as_str());
        create_dir(&mut s, &folder_dir)?;
        let mut dirs: BTreeSet<PathBuf> = BTreeSet::new();
        let mut entries = Vec::with_capacity(folder.files.len());
        for file in &folder.files {
            let dest = folder_dir.join(&file.rel_path);
            for dir in missing_dirs(&folder_dir, &dest, &dirs) {
                create_dir(&mut s, &dir)?;
                dirs.insert(dir);
            }
            let (size, sha256) = match &file.source {
                FileSource::ZipEntry(index) => {
                    let Some(zip) = archive.as_mut() else {
                        return Err(Error::internal("staging a zip entry without its blob"));
                    };
                    let entry = zip
                        .by_index(*index)
                        .map_err(|e| changed(&file.rel_path, &e.to_string()))?;
                    write(&mut s, &dest, entry)?
                }
                FileSource::Disk(path) => {
                    let source = std::fs::File::open(path)
                        .map_err(|e| changed(&file.rel_path, &e.to_string()))?;
                    write(&mut s, &dest, source)?
                }
            };
            if size != file.size || sha256 != file.sha256 {
                return Err(archive_error(
                    ArchiveFault::Malformed,
                    format!(
                        "{}/{} changed between inspection and extraction",
                        folder.name.as_str(),
                        file.rel_path
                    ),
                    json!({
                        "reason": "changed",
                        "path": format!("{}/{}", folder.name.as_str(), file.rel_path),
                        "expected": { "size": file.size, "sha256": file.sha256 },
                        "actual": { "size": size, "sha256": sha256 },
                    }),
                ));
            }
            let meta = std::fs::symlink_metadata(&dest)
                .map_err(|e| Error::io(&dest, &e, Suggestion::command(["omaddons", "status"])))?;
            entries.push(FileEntry {
                path: file.rel_path.clone(),
                size,
                mtime_ns: i128::from(meta.mtime()) * 1_000_000_000 + i128::from(meta.mtime_nsec()),
                sha256,
            });
            if first {
                first = false;
                failpoint::hit("engine.stage.file").map_err(fail)?;
            }
        }
        folders.push(StagedFolder {
            name: folder.name.clone(),
            path: folder_dir,
            index: FolderIndex::new(folder.name.as_str(), entries),
        });
    }

    fsx::sync_filesystem(&stage_dir)?;
    failpoint::hit("engine.stage.synced").map_err(fail)?;
    Ok(Staged {
        tx,
        stage_dir,
        folders,
    })
}

/// One directory under the stage dir, deferred and 0755.
fn create_dir(s: &mut WriteSession<'_>, dir: &Path) -> Result<()> {
    mutate_with(s, dir, Op::CreateDir, deferred())?;
    mutate(s, dir, Op::Chmod { mode: DIR_MODE })?;
    Ok(())
}

fn deferred() -> MutateOpts {
    MutateOpts {
        durability: Durability::Deferred,
        ..MutateOpts::default()
    }
}

/// The directories between `root` and `file` that are not created yet, outermost first.
fn missing_dirs(root: &Path, file: &Path, created: &BTreeSet<PathBuf>) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = file
        .ancestors()
        .skip(1)
        // ubs:ignore — comparing paths, not secrets
        .take_while(|dir| *dir != root && dir.starts_with(root))
        .filter(|dir| !created.contains(*dir))
        .map(Path::to_path_buf)
        .collect();
    dirs.reverse();
    dirs
}

/// Writes `source` to `dest` (0644, deferred) and returns what was written.
fn write(s: &mut WriteSession<'_>, dest: &Path, source: impl Read) -> Result<(u64, String)> {
    let mut tee = Tee {
        inner: source,
        hasher: Sha256::new(),
        len: 0,
    };
    let op = Op::WriteFile {
        content: Content::Reader(&mut tee),
        mode: Some(FILE_MODE),
    };
    mutate_with(s, dest, op, deferred())?;
    let sha: String = tee
        .hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    Ok((tee.len, sha))
}

/// Hashes and counts what passes through.
struct Tee<R> {
    inner: R,
    hasher: Sha256,
    len: u64,
}

impl<R: Read> Read for Tee<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        let chunk = buf.get(..n).unwrap_or_default();
        self.hasher.update(chunk);
        self.len += n as u64;
        Ok(n)
    }
}

fn open_zip(blob: &Path) -> Result<zip::ZipArchive<std::fs::File>> {
    let file = std::fs::File::open(blob)
        .map_err(|e| Error::io(blob, &e, Suggestion::text("download the archive again")))?;
    zip::ZipArchive::new(file).map_err(|e| changed("the archive", &e.to_string()))
}

/// The blob or directory no longer matches its inspection.
fn changed(what: &str, why: &str) -> Error {
    archive_error(
        ArchiveFault::Malformed,
        format!("{what} cannot be extracted as inspected: {why}"),
        json!({ "reason": "changed", "path": what }),
    )
}

/// The stage dir and AddOns (or `Interface/` when AddOns does not exist yet) must share
/// a filesystem, or the swap could not be a rename.
fn same_filesystem(stage_dir: &Path, client: &ClientPaths) -> Result<()> {
    let addons = client.addons();
    let other = if addons.exists() {
        addons
    } else {
        client.interface()
    };
    let dev = |p: &Path| {
        std::fs::metadata(p)
            .map(|m| m.dev())
            .map_err(|e| Error::io(p, &e, Suggestion::command(["omaddons", "status"])))
    };
    // ubs:ignore — comparing device numbers, not secrets
    if dev(stage_dir)? != dev(&other)? {
        return Err(Error::new(
            ErrorCode::Io,
            format!(
                "{} and {} are on different filesystems, so the swap cannot be an atomic rename; omaddons does not install across filesystems",
                stage_dir.display(),
                other.display()
            ),
            Suggestion::text(
                "keep Interface/ and Interface/AddOns/ on one filesystem (no symlink to another disk)",
            ),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::inspect::{ArchiveLimits, inspect_dir};
    use crate::fsx::{LockUse, PruneReason, TreeDigest, prune};
    use crate::rules::{ClientRules, ClientVersion, Family, InstallKind, RulesTable, TargetKind};
    use crate::state::paths::Paths;

    fn target(client_dir: &Path) -> Target {
        let mut target = Target {
            key: None,
            id: "retail".into(),
            install_root: client_dir.parent().unwrap().to_path_buf(),
            client_dir: client_dir.to_path_buf(),
            flavor_dir: Some("_retail_".into()),
            product: Some("wow".into()),
            install_kind: InstallKind::Battlenet,
            family: Family::Mainline,
            kind: TargetKind::Live,
            version: "12.1.5.70391".parse::<ClientVersion>().unwrap(),
            interface: 120_105,
            exe_name: None,
            label: None,
            rules: ClientRules::default(),
        };
        target.refresh_rules(&RulesTable::builtin());
        target
    }

    #[test]
    fn a_failpoint_mid_staging_leaves_addons_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let client = dir.path().join("_retail_");
        let addons = client.join("Interface/AddOns");
        std::fs::create_dir_all(addons.join("Existing")).unwrap();
        std::fs::write(
            addons.join("Existing/Existing.toc"),
            "## Interface: 120105\n",
        )
        .unwrap();
        let source = dir.path().join("src/Foo");
        std::fs::create_dir_all(&source).unwrap();
        for name in ["Foo.toc", "a.lua", "b.lua"] {
            std::fs::write(source.join(name), name).unwrap();
        }
        let paths = Paths::with_roots(
            dir.path(),
            dir.path().join("c"),
            dir.path().join("s"),
            dir.path().join("k"),
        );
        let scopes = ScopeTable::new(&paths, std::slice::from_ref(&client), None).unwrap();
        let inflight = InflightLock::create(&paths, &TxId::new()).unwrap();
        let inspected =
            inspect_dir(&source, &ArchiveLimits::default(), &RulesTable::builtin()).unwrap();
        let before = TreeDigest::compute(&addons).unwrap();

        let result = failpoint::with_spec("engine.stage.file=error", || {
            stage(&inflight, &scopes, &target(&client), None, &inspected)
        });
        let err = result.unwrap_err();
        assert_eq!(err.code, ErrorCode::Io, "{}", err.message);
        assert_eq!(
            TreeDigest::compute(&addons).unwrap(),
            before,
            "AddOns untouched"
        );
        let stage_dir = ClientPaths::new(&client).stage(&inflight.tx());
        assert!(
            stage_dir.join("Foo").is_dir(),
            "partial content stays for cleanup"
        );

        let report = prune(
            &scopes,
            PruneReason::PostCommitCleanup,
            std::slice::from_ref(&stage_dir),
            LockUse::Acquire,
        )
        .unwrap();
        assert!(!stage_dir.exists(), "{report:?}");
        assert_eq!(TreeDigest::compute(&addons).unwrap(), before);
    }

    #[test]
    fn missing_dirs_outermost_first() {
        let root = Path::new("/s/Foo");
        let mut created = BTreeSet::new();
        assert_eq!(
            missing_dirs(root, Path::new("/s/Foo/a/b/c.lua"), &created),
            [PathBuf::from("/s/Foo/a"), PathBuf::from("/s/Foo/a/b")]
        );
        created.insert(PathBuf::from("/s/Foo/a"));
        assert_eq!(
            missing_dirs(root, Path::new("/s/Foo/a/d.lua"), &created),
            Vec::<PathBuf>::new()
        );
        assert_eq!(
            missing_dirs(root, Path::new("/s/Foo/x.lua"), &created),
            Vec::<PathBuf>::new()
        );
    }

    #[test]
    fn the_tee_hashes_what_it_passes() {
        let mut tee = Tee {
            inner: &b"hello"[..],
            hasher: Sha256::new(),
            len: 0,
        };
        let mut out = Vec::new();
        tee.read_to_end(&mut out).unwrap();
        let sha: String = tee
            .hasher
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert_eq!((out.as_slice(), tee.len), (&b"hello"[..], 5));
        assert_eq!(
            sha,
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }

    #[test]
    fn tree_hash_format() {
        let files = vec![
            FileEntry {
                path: "b.lua".into(),
                size: 1,
                mtime_ns: 5,
                sha256: "bb".into(),
            },
            FileEntry {
                path: "a.toc".into(),
                size: 2,
                mtime_ns: 9,
                sha256: "aa".into(),
            },
        ];
        let mut hasher = Sha256::new();
        hasher.update(b"a.toc\x002\x00aa\nb.lua\x001\x00bb\n");
        let want: String = hasher
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let index = FolderIndex::new("Foo", files.clone());
        assert_eq!(
            index.tree,
            format!("tree:sha256:{want}"),
            "path-sorted, mtime excluded"
        );
        let mut shuffled = files;
        shuffled.reverse();
        shuffled[0].mtime_ns = 1;
        assert_eq!(FolderIndex::new("Foo", shuffled).tree, index.tree);
    }
}
