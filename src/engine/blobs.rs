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

use md5::Md5;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::error::{Error, ErrorCode, Result, Suggestion};
use crate::fsx::{
    self, Content, GlobalLock, InflightLock, LockUse, Op, PruneReason, RenameMode, ScopeTable,
    WriteSession, Writer, ensure_dir_all, mutate,
};
use crate::limits::DOWNLOAD_MAX_BYTES;
use crate::model::{ExpectedHash, HashFromHeader, TxId};
use crate::net::{Http, Request};
use crate::state::paths::Paths;

/// A stored archive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlobRef {
    /// Lowercase hex.
    pub sha256: String,
    pub size: u64,
    pub path: PathBuf,
}

/// Where one download attempt's bytes are, and what they must hash to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadTarget {
    pub url: String,
    pub declared: Option<ExpectedHash>,
    pub from_header: Option<HashFromHeader>,
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

    /// The download cap in force (`limits.download_max_bytes`; the engine passes the
    /// configured value, raised or lowered).
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
        let expected: Vec<ExpectedHash> = expected.into_iter().cloned().collect();
        self.store_checked(scopes, tx, reader, &expected, storage)
    }

    /// [`BlobCache::store_stream`] checking every hash in `expected`. SHA-256 is always
    /// computed (it names the blob); MD5 only when one is expected.
    pub fn store_checked(
        &self,
        scopes: &ScopeTable,
        tx: &TxId,
        reader: impl Read,
        expected: &[ExpectedHash],
        storage: &Storage,
    ) -> Result<BlobRef> {
        let wants_md5 = expected.iter().any(|e| matches!(e, ExpectedHash::Md5(_)));
        let mut guard = Guard {
            inner: reader,
            hasher: Sha256::new(),
            md5: wants_md5.then(Md5::new),
            len: 0,
            max: self.max_download,
            expected: expected
                .iter()
                .map(|e| match e {
                    ExpectedHash::Sha256(hex) => ExpectedHash::Sha256(hex.to_ascii_lowercase()),
                    ExpectedHash::Md5(hex) => ExpectedHash::Md5(hex.to_ascii_lowercase()),
                })
                .collect(),
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

    /// Downloads `url` into the cache, checking `declared` and, when `from_header` says
    /// so and no MD5 is declared, the MD5 the response's header carries. A hash
    /// mismatch is retried once; a second one is not retryable. A header that cannot be
    /// used is no check, said in `notes` (for `-v`).
    #[allow(clippy::too_many_arguments)]
    pub fn download(
        &self,
        scopes: &ScopeTable,
        tx: &TxId,
        http: &dyn Http,
        url: &str,
        declared: Option<&ExpectedHash>,
        from_header: Option<HashFromHeader>,
        notes: &mut Vec<String>,
    ) -> Result<BlobRef> {
        let target = DownloadTarget {
            url: url.to_owned(),
            declared: declared.cloned(),
            from_header,
        };
        self.download_resolving(scopes, tx, http, &mut || Ok(target.clone()), false, notes)
    }

    /// [`BlobCache::download`] from a target `resolve` gives afresh before each attempt
    /// (a deferred asset's token URL). The one retry follows a hash mismatch or, when
    /// `refreshable`, a 403 or 404 (the token died), so `resolve` runs at most twice.
    pub fn download_resolving(
        &self,
        scopes: &ScopeTable,
        tx: &TxId,
        http: &dyn Http,
        resolve: &mut dyn FnMut() -> Result<DownloadTarget>,
        refreshable: bool,
        notes: &mut Vec<String>,
    ) -> Result<BlobRef> {
        let mut attempt = 1;
        loop {
            let target = resolve()?;
            let resp = http.send(Request::get(&target.url, "asset").download())?;
            if refreshable && matches!(resp.status, 403 | 404) && attempt == 1 {
                attempt += 1;
                continue;
            }
            let resp = resp.error_for_status("asset")?;
            let declared_md5 = matches!(target.declared, Some(ExpectedHash::Md5(_)));
            let mut expected: Vec<ExpectedHash> = target.declared.into_iter().collect();
            if let (Some(HashFromHeader::EtagMd5), false) = (target.from_header, declared_md5) {
                match etag_md5(resp.header("etag")) {
                    Ok(md5) => expected.push(md5),
                    Err(why) => notes.push(format!(
                        "{}: {why}; its MD5 is not checked",
                        crate::net::redact(&target.url)
                    )),
                }
            }
            match self.store_checked(scopes, tx, resp.into_reader(), &expected, &Storage::Cache) {
                Err(e) if e.code == ErrorCode::HashMismatch && attempt == 1 => attempt += 1,
                Err(e) if e.code == ErrorCode::HashMismatch => return Err(e.retryable(false)),
                done => return done,
            }
        }
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
/// other than one `expected`; the failure is kept for the caller.
struct Guard<R> {
    inner: R,
    hasher: Sha256,
    /// Only when an MD5 is expected.
    md5: Option<Md5>,
    len: u64,
    max: u64,
    expected: Vec<ExpectedHash>,
    failure: Option<Error>,
}

impl<R> Guard<R> {
    fn hex(&self) -> String {
        hex(&self.hasher.clone().finalize())
    }

    /// What the stream hashed to in `expected`'s algorithm.
    fn actual(&self, expected: &ExpectedHash) -> Option<String> {
        match expected {
            ExpectedHash::Sha256(_) => Some(self.hex()),
            ExpectedHash::Md5(_) => self.md5.as_ref().map(|m| hex(&m.clone().finalize())),
        }
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
            for expected in self.expected.clone() {
                let actual = self.actual(&expected).unwrap_or_default();
                let published = expected.hex();
                // ubs:ignore — comparing a download's digest with its published one, not a secret
                if actual != published {
                    let algorithm = expected.algorithm();
                    return Err(self.fail(
                        Error::hash_mismatch(
                            1,
                            format!("the download's {algorithm} is {actual}, but the source published {published}"),
                            Suggestion::text("retry once; if it persists, the upstream file changed or is corrupt"),
                        )
                        .details(json!({ "expected": published, "actual": actual })),
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
        let chunk = buf.get(..n).unwrap_or_default();
        self.hasher.update(chunk);
        if let Some(md5) = &mut self.md5 {
            md5.update(chunk);
        }
        Ok(n)
    }
}

/// The MD5 a strong, quoted, 32-hex `ETag` carries; otherwise why there is none to
/// check (absent, weak, unquoted, or not an MD5).
fn etag_md5(etag: Option<&str>) -> std::result::Result<ExpectedHash, String> {
    let Some(etag) = etag.map(str::trim) else {
        return Err("no ETag".to_owned());
    };
    if etag.starts_with("W/") {
        return Err(format!("a weak ETag ({etag})"));
    }
    let quoted = etag
        .strip_prefix('"')
        .and_then(|e| e.strip_suffix('"'))
        .ok_or_else(|| format!("an unquoted ETag ({etag})"))?;
    ExpectedHash::md5(quoted).ok_or_else(|| format!("an ETag that is not an MD5 ({etag})"))
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
    fn a_raised_download_cap_from_config_is_honoured() {
        use crate::engine::inspect::ArchiveLimits;
        use crate::limits::EffectiveLimits;

        // config.toml raised limits.download_max_bytes past what was the cap.
        let raised = EffectiveLimits {
            download_max_bytes: 2_000,
            ..EffectiveLimits::default()
        };
        let limits = ArchiveLimits::from(&raised);
        assert_eq!(limits.max_download, 2_000);
        let f = fixture();
        let cache = BlobCache::new(&f.paths).with_max_download(limits.max_download);
        let stored =
            cache.store_stream(&f.scopes, &TxId::new(), Zeros(1_500), None, &Storage::Cache);
        assert!(stored.is_ok(), "{stored:?}");
        let err = cache
            .store_stream(&f.scopes, &TxId::new(), Zeros(2_001), None, &Storage::Cache)
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::ArchiveTooLarge);
        assert_eq!(ArchiveLimits::default().max_download, DOWNLOAD_MAX_BYTES);
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

    fn md5(bytes: &[u8]) -> String {
        hex(&Md5::digest(bytes))
    }

    #[test]
    fn md5_expectations_are_checked_and_blobs_stay_keyed_by_sha256() {
        let f = fixture();
        let cache = BlobCache::new(&f.paths);
        let good = ExpectedHash::md5(&md5(b"abc").to_uppercase()).unwrap();
        let blob = cache
            .store_stream(
                &f.scopes,
                &TxId::new(),
                Cursor::new(b"abc".to_vec()),
                Some(&good),
                &Storage::Cache,
            )
            .unwrap();
        assert_eq!(blob.sha256, sha(b"abc"));
        assert!(f.paths.blob(&sha(b"abc")).is_file());

        let bad = ExpectedHash::md5(&md5(b"xyz")).unwrap();
        let err = cache
            .store_stream(
                &f.scopes,
                &TxId::new(),
                Cursor::new(b"def".to_vec()),
                Some(&bad),
                &Storage::Cache,
            )
            .unwrap_err();
        assert_eq!((err.code, err.retryable), (ErrorCode::HashMismatch, true));
        assert!(err.message.contains("MD5"), "{}", err.message);
        assert_eq!(
            (&err.details["expected"], &err.details["actual"]),
            (&json!(md5(b"xyz")), &json!(md5(b"def")))
        );
        assert!(!f.paths.blob(&sha(b"def")).exists());

        // Both algorithms at once: each is checked.
        let both = [
            ExpectedHash::sha256(&sha(b"ghi")).unwrap(),
            ExpectedHash::md5(&md5(b"nope")).unwrap(),
        ];
        let err = cache
            .store_checked(
                &f.scopes,
                &TxId::new(),
                Cursor::new(b"ghi".to_vec()),
                &both,
                &Storage::Cache,
            )
            .unwrap_err();
        assert!(err.message.contains("MD5"), "{}", err.message);
    }

    /// Serves `body` with `etag`, counting requests.
    struct Serve {
        body: Vec<u8>,
        etag: Option<String>,
        sent: AtomicU64,
    }

    impl Serve {
        fn new(body: &[u8], etag: Option<&str>) -> Self {
            Self {
                body: body.to_vec(),
                etag: etag.map(str::to_owned),
                sent: AtomicU64::new(0),
            }
        }
    }

    impl Http for Serve {
        fn send(&self, req: Request) -> Result<crate::net::Response> {
            self.sent.fetch_add(1, Ordering::Relaxed);
            let headers = self
                .etag
                .iter()
                .map(|e| ("ETag".to_owned(), e.clone()))
                .collect();
            Ok(crate::net::Response::new(
                200,
                headers,
                req.url,
                Box::new(Cursor::new(self.body.clone())),
            ))
        }
    }

    #[test]
    fn a_strong_etag_md5_is_checked_and_an_unusable_one_is_noted() {
        let f = fixture();
        let cache = BlobCache::new(&f.paths);
        let url = "https://www.tukui.org/download/elvui.zip";
        let download = |http: &Serve, declared: Option<&ExpectedHash>| {
            let mut notes = Vec::new();
            let got = cache.download(
                &f.scopes,
                &TxId::new(),
                http,
                url,
                declared,
                Some(HashFromHeader::EtagMd5),
                &mut notes,
            );
            (got, notes)
        };
        // A matching ETag verifies.
        let quoted = format!("\"{}\"", md5(b"elvui"));
        let (got, notes) = download(&Serve::new(b"elvui", Some(&quoted)), None);
        assert_eq!(got.unwrap().sha256, sha(b"elvui"));
        assert!(notes.is_empty(), "{notes:?}");

        // A wrong one fails, retried once; the second failure is not retryable.
        let wrong = format!("\"{}\"", md5(b"other"));
        let http = Serve::new(b"elvui", Some(&wrong));
        let (got, _) = download(&http, None);
        let err = got.unwrap_err();
        assert_eq!((err.code, err.retryable), (ErrorCode::HashMismatch, false));
        assert_eq!(http.sent.load(Ordering::Relaxed), 2);

        // Weak, absent, unquoted or not an MD5: no check, and a note says so.
        let plain = md5(b"elvui");
        for etag in [
            Some("W/\"abc\""),
            None,
            Some(plain.as_str()),
            Some("\"abc\""),
        ] {
            let (got, notes) = download(&Serve::new(b"elvui", etag), None);
            assert!(got.is_ok(), "{etag:?}");
            assert_eq!(notes.len(), 1, "{etag:?}: {notes:?}");
            assert!(notes[0].contains("not checked"), "{notes:?}");
        }

        // A declared MD5 wins; the header is not consulted.
        let declared = ExpectedHash::md5(&md5(b"elvui")).unwrap();
        let (got, notes) = download(&Serve::new(b"elvui", Some(&wrong)), Some(&declared));
        assert!(got.is_ok() && notes.is_empty());
    }

    /// Answers each URL its route names (404 otherwise): `(status, ETag)`, serving
    /// `elvui` on a 200.
    struct Routes(Vec<(String, u16, Option<String>)>);

    impl Http for Routes {
        fn send(&self, req: Request) -> Result<crate::net::Response> {
            // ubs:ignore — matching fixture URLs, not secrets
            let route = self.0.iter().find(|(url, ..)| *url == req.url);
            let (status, etag) = route.map_or((404, None), |(_, s, e)| (*s, e.clone()));
            let body = if status == 200 {
                b"elvui".to_vec()
            } else {
                Vec::new()
            };
            Ok(crate::net::Response::new(
                status,
                etag.map(|e| ("ETag".to_owned(), e)).into_iter().collect(),
                req.url,
                Box::new(Cursor::new(body)),
            ))
        }
    }

    #[test]
    fn a_resolved_download_resolves_again_before_its_one_retry() {
        let f = fixture();
        let cache = BlobCache::new(&f.paths);
        let good = format!("\"{}\"", md5(b"elvui"));
        let wrong = format!("\"{}\"", md5(b"other"));
        let base = "https://api.tukui.org/v1/download/elvui/token";
        // Token n answers `answers[n - 1]`; returns the result and how many resolves.
        let run = |answers: &[(u16, Option<&str>)], refreshable: bool| {
            let routes = answers
                .iter()
                .enumerate()
                .map(|(i, (status, etag))| {
                    (format!("{base}{}", i + 1), *status, etag.map(str::to_owned))
                })
                .collect();
            let http = Routes(routes);
            let mut resolves = 0;
            let mut resolve = || {
                resolves += 1;
                Ok(DownloadTarget {
                    url: format!("{base}{resolves}"),
                    declared: None,
                    from_header: Some(HashFromHeader::EtagMd5),
                })
            };
            let got = cache.download_resolving(
                &f.scopes,
                &TxId::new(),
                &http,
                &mut resolve,
                refreshable,
                &mut Vec::new(),
            );
            (got, resolves)
        };

        // A dead token (403 or 404), or a hash mismatch, is resolved again once.
        for first in [(403, None), (404, None), (200, Some(wrong.as_str()))] {
            let (got, resolves) = run(&[first, (200, Some(&good))], true);
            assert_eq!(got.unwrap().sha256, sha(b"elvui"), "{first:?}");
            assert_eq!(resolves, 2, "{first:?}");
        }
        // Dead twice: the second answer is the error, after two resolves.
        let (got, resolves) = run(&[(403, None), (403, None), (200, Some(&good))], true);
        assert_eq!(got.unwrap_err().code, ErrorCode::HttpStatus);
        assert_eq!(resolves, 2);
        // A mismatch twice is not retryable.
        let (got, resolves) = run(&[(200, Some(&wrong)), (200, Some(&wrong))], true);
        let err = got.unwrap_err();
        assert_eq!(
            (err.code, err.retryable, resolves),
            (ErrorCode::HashMismatch, false, 2)
        );
        // A fixed URL is not refreshed: a 403 fails at once.
        let (got, resolves) = run(&[(403, None), (200, Some(&good))], false);
        assert_eq!(
            (got.unwrap_err().code, resolves),
            (ErrorCode::HttpStatus, 1)
        );
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
