//! The content-addressed blob cache (vision §5.5 step 2): every downloaded archive is
//! stored once as `<cache>/omaddons/blobs/<sha256>`, however many targets or
//! transactions use it, so reinstalls, undo and cross-target copies work offline.
//!
//! A download streams into `<cache>/omaddons/tmp/<tx>/blob-<n>` through fsx's atomic
//! write, hashed and size-capped on the way (`DOWNLOAD_MAX_BYTES`), and an expected
//! hash is checked at the end of the stream: any failure aborts the atomic write, so
//! nothing is left behind. The finished file is renamed onto its hash name; a blob
//! already there has the same bytes by construction, so the rename simply replaces it
//! and concurrent stores of the same content end as one file.
//!
//! Least-recently-used eviction keeps the cache under `BLOB_CACHE_MAX_BYTES`, never
//! touching pinned blobs (installed, retained or planned). Last use lives in
//! `blobs/index.json` (atime is often off); the index is derived and rebuilt from the
//! directory when missing or corrupt. CurseForge and Wago archives (M5) must never be
//! cached: [`Storage::Uncached`] writes them to the transaction's stage instead.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Read};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::error::{Error, ErrorCode, Result, Suggestion};
use crate::fsx::{
    self, Content, GlobalLock, InflightLock, LockUse, Op, PruneReason, RenameMode, ScopeTable,
    WriteSession, Writer, ensure_dir_all, mutate,
};
use crate::limits::DOWNLOAD_MAX_BYTES;
use crate::model::{ExpectedHash, TxId};
use crate::state::paths::Paths;

/// A stored archive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlobRef {
    /// Lowercase hex.
    pub sha256: String,
    pub size: u64,
    pub path: PathBuf,
}

/// Where a download may be kept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Storage {
    /// In the shared blob cache.
    Cache,
    /// At `dest` only (CurseForge and Wago, whose terms forbid caching): typically a
    /// file in the transaction's stage dir.
    Uncached { dest: PathBuf },
}

/// What an eviction pass did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EvictReport {
    /// Evicted blobs, least recently used first.
    pub evicted: Vec<String>,
    pub freed_bytes: u64,
    /// Bytes left in the cache.
    pub remaining_bytes: u64,
    /// Still over the cap because the rest is pinned.
    pub over_cap: bool,
    /// Temp dirs of dead transactions removed.
    pub orphans_removed: usize,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
struct IndexEntry {
    size: u64,
    last_used_ns: i128,
}

/// The cache under one set of paths.
pub struct BlobCache {
    paths: Paths,
    max_download: u64,
    clock: fn() -> i128,
    counter: AtomicU64,
}

impl BlobCache {
    pub fn new(paths: &Paths) -> Self {
        Self {
            paths: paths.clone(),
            max_download: DOWNLOAD_MAX_BYTES,
            clock: now_ns,
            counter: AtomicU64::new(0),
        }
    }

    /// A smaller download cap (tests; production uses `DOWNLOAD_MAX_BYTES`).
    #[must_use]
    pub fn with_max_download(mut self, bytes: u64) -> Self {
        self.max_download = bytes;
        self
    }

    /// Another clock for last-use times (tests).
    #[must_use]
    pub fn with_clock(mut self, clock: fn() -> i128) -> Self {
        self.clock = clock;
        self
    }

    /// Streams `reader` into storage, hashing it, capping its size and checking
    /// `expected`. Errors: `archive_too_large` (exit 4), `hash_mismatch` (exit 1,
    /// retryable: the caller may download once more and should mark a second failure
    /// not retryable).
    pub fn store_stream(
        &self,
        scopes: &ScopeTable,
        tx: &TxId,
        reader: impl Read,
        expected: Option<&ExpectedHash>,
        storage: &Storage,
    ) -> Result<BlobRef> {
        let mut guard = Guard {
            inner: reader,
            hasher: Sha256::new(),
            len: 0,
            max: self.max_download,
            expected: expected.map(|ExpectedHash::Sha256(hex)| hex.to_ascii_lowercase()),
            failure: None,
        };
        let (written_to, writer) = match storage {
            Storage::Cache => {
                let n = self.counter.fetch_add(1, Ordering::Relaxed);
                (
                    self.paths.cache_tmp(tx).join(format!("blob-{n}")),
                    Writer::Derived,
                )
            }
            Storage::Uncached { dest } => (dest.clone(), Writer::Mutating),
        };
        let mut s = WriteSession::untracked(writer, scopes, "blob");
        if let Some(parent) = written_to.parent() {
            ensure_dir_all(&mut s, parent)?;
        }
        let op = Op::WriteFile {
            content: Content::Reader(&mut guard),
            mode: Some(0o644),
        };
        if let Err(err) = mutate(&mut s, &written_to, op) {
            return Err(guard.failure.take().unwrap_or_else(|| err.into()));
        }
        let sha256 = guard.hex();
        let size = guard.len;
        let path = match storage {
            Storage::Uncached { dest } => dest.clone(),
            Storage::Cache => {
                let blobs = self.paths.blobs_dir();
                let mut s = WriteSession::untracked(Writer::Derived, scopes, "blob");
                ensure_dir_all(&mut s, &blobs)?;
                let path = self.paths.blob(&sha256);
                let op = Op::Rename {
                    to: &path,
                    mode: RenameMode::Replace,
                };
                mutate(&mut s, &written_to, op)?;
                self.touch(scopes, &sha256, size)?;
                path
            }
        };
        Ok(BlobRef { sha256, size, path })
    }

    /// The cached blob with this hash, marking it used.
    pub fn get(&self, scopes: &ScopeTable, sha256: &str) -> Result<Option<BlobRef>> {
        let sha256 = sha256.to_ascii_lowercase();
        if !is_sha256(&sha256) {
            return Ok(None);
        }
        let path = self.paths.blob(&sha256);
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            return Ok(None);
        };
        if !meta.is_file() {
            return Ok(None);
        }
        self.touch(scopes, &sha256, meta.len())?;
        Ok(Some(BlobRef {
            sha256,
            size: meta.len(),
            path,
        }))
    }

    /// Opens a blob for reading.
    pub fn open(&self, blob: &BlobRef) -> Result<std::fs::File> {
        std::fs::File::open(&blob.path).map_err(|e| {
            Error::io(
                &blob.path,
                &e,
                Suggestion::text("download the archive again"),
            )
        })
    }

    /// Evicts least-recently-used unpinned blobs until the cache is at most `cap`
    /// bytes, and removes temp dirs of transactions that are no longer alive.
    pub fn evict(
        &self,
        scopes: &ScopeTable,
        pins: &BTreeSet<String>,
        cap: u64,
    ) -> Result<EvictReport> {
        let index = self.index();
        let mut blobs: Vec<(String, u64, i128)> = list(&self.paths.blobs_dir())
            .into_iter()
            .filter(|(name, _)| is_sha256(name))
            .map(|(name, meta)| {
                let last = index
                    .get(&name)
                    .map_or_else(|| mtime_ns(&meta), |e| e.last_used_ns);
                (name, meta.len(), last)
            })
            .collect();
        blobs.sort_by(|a, b| a.2.cmp(&b.2).then_with(|| a.0.cmp(&b.0)));
        let mut report = EvictReport {
            remaining_bytes: blobs.iter().map(|b| b.1).sum(),
            ..EvictReport::default()
        };
        for (name, size, _) in &blobs {
            if report.remaining_bytes <= cap {
                break;
            }
            if pins.contains(name) {
                continue;
            }
            report.evicted.push(name.clone());
            report.freed_bytes += size;
            report.remaining_bytes -= size;
        }
        report.over_cap = report.remaining_bytes > cap;

        let orphans: Vec<PathBuf> = list(&self.paths.cache_dir().join("tmp"))
            .into_iter()
            .filter_map(|(name, _)| name.parse::<TxId>().ok())
            .filter(|tx| !InflightLock::is_live(&self.paths, tx).unwrap_or(true))
            .map(|tx| self.paths.cache_tmp(&tx))
            .collect();
        if report.evicted.is_empty() && orphans.is_empty() {
            return Ok(report);
        }
        let lock = GlobalLock::acquire(&self.paths, "evict")?;
        if !report.evicted.is_empty() {
            let victims: Vec<PathBuf> = report
                .evicted
                .iter()
                .map(|sha| self.paths.blob(sha))
                .collect();
            fsx::prune(
                scopes,
                PruneReason::Retention,
                &victims,
                LockUse::Held(&lock),
            )?;
            let mut index = self.index();
            for sha in &report.evicted {
                index.remove(sha);
            }
            self.save_index(scopes, &index)?;
        }
        if !orphans.is_empty() {
            let pruned = fsx::prune(
                scopes,
                PruneReason::PostCommitCleanup,
                &orphans,
                LockUse::Held(&lock),
            )?;
            report.orphans_removed = pruned.removed.len();
        }
        Ok(report)
    }

    /// The last-use index; rebuilt from the directory (mtimes) when missing or corrupt.
    fn index(&self) -> BTreeMap<String, IndexEntry> {
        let path = self.paths.blobs_dir().join("index.json");
        if let Some(index) = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
        {
            return index;
        }
        list(&self.paths.blobs_dir())
            .into_iter()
            .filter(|(name, _)| is_sha256(name))
            .map(|(name, meta)| {
                let entry = IndexEntry {
                    size: meta.len(),
                    last_used_ns: mtime_ns(&meta),
                };
                (name, entry)
            })
            .collect()
    }

    fn touch(&self, scopes: &ScopeTable, sha256: &str, size: u64) -> Result<()> {
        let mut index = self.index();
        index.insert(
            sha256.to_owned(),
            IndexEntry {
                size,
                last_used_ns: (self.clock)(),
            },
        );
        self.save_index(scopes, &index)
    }

    fn save_index(&self, scopes: &ScopeTable, index: &BTreeMap<String, IndexEntry>) -> Result<()> {
        let bytes = serde_json::to_vec(index)
            .map_err(|e| Error::internal(format!("serializing the blob index: {e}")))?;
        let mut s = WriteSession::untracked(Writer::Derived, scopes, "blob index");
        ensure_dir_all(&mut s, &self.paths.blobs_dir())?;
        let op = Op::WriteFile {
            content: Content::Bytes(&bytes),
            mode: Some(0o644),
        };
        mutate(&mut s, &self.paths.blobs_dir().join("index.json"), op)?;
        Ok(())
    }
}

/// Hashes and counts the stream, failing past `max` bytes or, at its end, on a hash
/// other than `expected`; the failure is kept for the caller.
struct Guard<R> {
    inner: R,
    hasher: Sha256,
    len: u64,
    max: u64,
    expected: Option<String>,
    failure: Option<Error>,
}

impl<R> Guard<R> {
    fn hex(&self) -> String {
        hex(&self.hasher.clone().finalize())
    }

    fn fail(&mut self, error: Error) -> io::Error {
        let message = error.message.clone();
        self.failure = Some(error);
        io::Error::other(message)
    }
}

impl<R: Read> Read for Guard<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        if n == 0 {
            if let Some(expected) = self.expected.clone() {
                let actual = self.hex();
                // ubs:ignore — comparing a download's digest with its published one, not a secret
                if actual != expected {
                    return Err(self.fail(
                        Error::hash_mismatch(
                            1,
                            format!("the download's SHA-256 is {actual}, but the source published {expected}"),
                            Suggestion::text("retry once; if it persists, the upstream file changed or is corrupt"),
                        )
                        .details(json!({ "expected": expected, "actual": actual })),
                    ));
                }
            }
            return Ok(0);
        }
        self.len += n as u64;
        if self.len > self.max {
            let max = self.max;
            return Err(self.fail(
                Error::new(
                    ErrorCode::ArchiveTooLarge,
                    format!("the download is larger than the {max}-byte limit"),
                    Suggestion::text(
                        "if the archive is genuine, raise the limit: omaddons config set limits.download_max_bytes <value>",
                    ),
                )
                .details(json!({ "limit": max, "config_key": "limits.download_max_bytes" })),
            ));
        }
        self.hasher.update(buf.get(..n).unwrap_or_default());
        Ok(n)
    }
}

fn is_sha256(name: &str) -> bool {
    name.len() == 64
        && name
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Entry names and metadata of `dir`; nothing when it cannot be read.
fn list(dir: &Path) -> Vec<(String, std::fs::Metadata)> {
    let Ok(listing) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    listing
        .filter_map(std::result::Result::ok)
        .filter_map(|e| Some((e.file_name().into_string().ok()?, e.metadata().ok()?)))
        .collect()
}

fn mtime_ns(meta: &std::fs::Metadata) -> i128 {
    i128::from(meta.mtime()) * 1_000_000_000 + i128::from(meta.mtime_nsec())
}

fn now_ns() -> i128 {
    jiff::Timestamp::now().as_nanosecond()
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    struct Fixture {
        _dir: tempfile::TempDir,
        paths: Paths,
        scopes: ScopeTable,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let paths = Paths::with_roots(&root, root.join("c"), root.join("s"), root.join("k"));
        let scopes = ScopeTable::new(&paths, &[], None).unwrap();
        Fixture {
            _dir: dir,
            paths,
            scopes,
        }
    }

    fn sha(bytes: &[u8]) -> String {
        hex(&Sha256::digest(bytes))
    }

    /// Every file under `dir`, relative.
    fn files(dir: &Path) -> Vec<String> {
        let mut out = Vec::new();
        let mut pending = vec![dir.to_path_buf()];
        while let Some(at) = pending.pop() {
            for (name, meta) in list(&at) {
                let path = at.join(&name);
                if meta.is_dir() {
                    pending.push(path);
                } else {
                    out.push(path.strip_prefix(dir).unwrap().display().to_string());
                }
            }
        }
        out.sort();
        out
    }

    #[test]
    fn identical_content_is_stored_once() {
        let f = fixture();
        let cache = BlobCache::new(&f.paths);
        let tx = TxId::new();
        let a = cache
            .store_stream(
                &f.scopes,
                &tx,
                Cursor::new(b"zip bytes".to_vec()),
                None,
                &Storage::Cache,
            )
            .unwrap();
        let b = cache
            .store_stream(
                &f.scopes,
                &TxId::new(),
                Cursor::new(b"zip bytes".to_vec()),
                None,
                &Storage::Cache,
            )
            .unwrap();
        assert_eq!(a, b);
        assert_eq!(a.sha256, sha(b"zip bytes"));
        assert_eq!(a.size, 9);
        assert_eq!(
            files(&f.paths.blobs_dir()),
            [a.sha256.clone(), "index.json".to_owned()]
        );
        assert!(
            files(&f.paths.cache_dir().join("tmp")).is_empty(),
            "no temp file left"
        );
        assert_eq!(
            cache.get(&f.scopes, &a.sha256.to_uppercase()).unwrap(),
            Some(a.clone())
        );
        assert_eq!(cache.get(&f.scopes, &"0".repeat(64)).unwrap(), None);
        let mut read = String::new();
        cache.open(&a).unwrap().read_to_string(&mut read).unwrap();
        assert_eq!(read, "zip bytes");
    }

    /// Yields `n` bytes without allocating them.
    struct Zeros(u64);

    impl Read for Zeros {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let n = buf.len().min(usize::try_from(self.0).unwrap_or(usize::MAX));
            buf[..n].fill(0);
            self.0 -= n as u64;
            Ok(n)
        }
    }

    #[test]
    fn oversized_downloads_leave_nothing() {
        let f = fixture();
        let cache = BlobCache::new(&f.paths).with_max_download(1_000);
        let err = cache
            .store_stream(&f.scopes, &TxId::new(), Zeros(1_001), None, &Storage::Cache)
            .unwrap_err();
        assert_eq!(
            (err.code, err.exit().code()),
            (ErrorCode::ArchiveTooLarge, 4)
        );
        assert_eq!(err.details["config_key"], "limits.download_max_bytes");
        assert!(
            files(f.paths.cache_dir()).is_empty(),
            "{:?}",
            files(f.paths.cache_dir())
        );
        assert!(
            cache
                .store_stream(&f.scopes, &TxId::new(), Zeros(1_000), None, &Storage::Cache)
                .is_ok()
        );
        assert_eq!(DOWNLOAD_MAX_BYTES, 512 << 20, "the production cap");
    }

    #[test]
    fn expected_hashes() {
        let f = fixture();
        let cache = BlobCache::new(&f.paths);
        let good = ExpectedHash::Sha256(sha(b"abc").to_uppercase());
        assert!(
            cache
                .store_stream(
                    &f.scopes,
                    &TxId::new(),
                    Cursor::new(b"abc".to_vec()),
                    Some(&good),
                    &Storage::Cache
                )
                .is_ok()
        );
        let bad = ExpectedHash::Sha256(sha(b"xyz"));
        let err = cache
            .store_stream(
                &f.scopes,
                &TxId::new(),
                Cursor::new(b"def".to_vec()),
                Some(&bad),
                &Storage::Cache,
            )
            .unwrap_err();
        assert_eq!(
            (err.code, err.exit().code(), err.retryable),
            (ErrorCode::HashMismatch, 1, true)
        );
        assert_eq!(err.details["actual"], sha(b"def"));
        assert!(files(&f.paths.cache_dir().join("tmp")).is_empty());
        assert!(!f.paths.blob(&sha(b"def")).exists());
    }

    #[test]
    fn uncached_storage_never_touches_the_cache() {
        let f = fixture();
        let cache = BlobCache::new(&f.paths);
        let client = tempfile::tempdir().unwrap();
        let client_dir = std::fs::canonicalize(client.path())
            .unwrap()
            .join("_retail_");
        std::fs::create_dir_all(client_dir.join("Interface/AddOns")).unwrap();
        let scopes = ScopeTable::new(&f.paths, std::slice::from_ref(&client_dir), None).unwrap();
        let tx = TxId::new();
        let dest = crate::state::paths::ClientPaths::new(&client_dir)
            .stage(&tx)
            .join("download.zip");
        let blob = cache
            .store_stream(
                &scopes,
                &tx,
                Cursor::new(b"curse".to_vec()),
                None,
                &Storage::Uncached { dest: dest.clone() },
            )
            .unwrap();
        assert_eq!((blob.path, blob.sha256), (dest.clone(), sha(b"curse")));
        assert!(dest.is_file());
        assert!(
            !f.paths.blobs_dir().exists() && !f.paths.cache_dir().join("tmp").exists(),
            "{:?}",
            files(f.paths.cache_dir())
        );
    }

    fn at_1() -> i128 {
        1
    }

    fn at_2() -> i128 {
        2
    }

    fn at_3() -> i128 {
        3
    }

    #[test]
    fn least_recently_used_unpinned_blobs_go_first() {
        let f = fixture();
        let store = |clock: fn() -> i128, bytes: &[u8]| {
            BlobCache::new(&f.paths)
                .with_clock(clock)
                .store_stream(
                    &f.scopes,
                    &TxId::new(),
                    Cursor::new(bytes.to_vec()),
                    None,
                    &Storage::Cache,
                )
                .unwrap()
                .sha256
        };
        let (old, mid, new) = (
            store(at_1, b"aaaa"),
            store(at_2, b"bbbb"),
            store(at_3, b"cccc"),
        );
        let cache = BlobCache::new(&f.paths);
        let report = cache
            .evict(&f.scopes, &BTreeSet::from([old.clone()]), 8)
            .unwrap();
        assert_eq!(
            report.evicted,
            std::slice::from_ref(&mid),
            "old is pinned, mid is next"
        );
        assert_eq!(
            (report.freed_bytes, report.remaining_bytes, report.over_cap),
            (4, 8, false)
        );
        assert!(
            f.paths.blob(&old).exists()
                && !f.paths.blob(&mid).exists()
                && f.paths.blob(&new).exists()
        );

        let report = cache
            .evict(&f.scopes, &BTreeSet::from([old.clone(), new.clone()]), 1)
            .unwrap();
        assert!(report.evicted.is_empty());
        assert!(report.over_cap, "pinned blobs stay even over the cap");
    }

    #[test]
    fn a_corrupt_index_is_rebuilt() {
        let f = fixture();
        let cache = BlobCache::new(&f.paths);
        let blob = cache
            .store_stream(
                &f.scopes,
                &TxId::new(),
                Cursor::new(b"x".to_vec()),
                None,
                &Storage::Cache,
            )
            .unwrap();
        let index = f.paths.blobs_dir().join("index.json");
        std::fs::write(&index, "{ not json").unwrap();
        assert_eq!(cache.index().keys().collect::<Vec<_>>(), [&blob.sha256]);
        std::fs::remove_file(&index).unwrap();
        assert!(cache.get(&f.scopes, &blob.sha256).unwrap().is_some());
        assert!(index.is_file());
        assert_eq!(
            cache.evict(&f.scopes, &BTreeSet::new(), 0).unwrap().evicted,
            [blob.sha256]
        );
    }
}
